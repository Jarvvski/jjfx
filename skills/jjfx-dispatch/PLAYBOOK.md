# Driving a Ticket backlog with jjfx - the agent playbook

`SKILL.md` is the command surface and the intent. This is the working guide: the
loop as it actually runs, the traps a drive hits, the workaround for each, and -
the point - **what jjfx should be doing for the agent rather than asking it to
remember**. It is written from the second dogfood drive (eight Tickets in a Python
repo, a four-Worker pool, a real GitHub forge); the findings it refers to are
under `.scratch/dogfood-2/issues/`.

Read this before the first `dispatch`. The traps are ordered by how early they
bite.

---

## 1. Pin the binary (before anything else)

`jjfx` on this machine is a stale `0.54.0` first on `PATH`; the verbs a drive uses
(`resume`, `ticket list`) do not exist in it. Run the build you mean, and verify:

```sh
export PATH="/Users/jarvis/Code/personal/jjfx/target/release:$PATH"
jjfx version                       # must be the build under test, not a release
jjfx help dispatch; jjfx help resume; jjfx help ticket list
```

If a verb the playbook uses does not answer, **stop** - you are on the wrong
binary. (`dogfood/07` documents this exact trap.)

**What jjfx should own:** nothing an agent must remember. The skill already says
to check which binary you are driving; a `jjfx` that detects a version too old for
the command being run and says so would be better than a warning in prose.

---

## 2. The loop

```sh
jjfx root                     # the repository jjfx creates Workers beside
jjfx colocation status        # colocated Workers carry a working .git
jjfx hooks status             # which runtimes report lifecycle; see §3
jjfx pool profile opencode    # safe before the pool exists
jjfx pool 4                   # capacity; a safe shrink
jjfx ticket list              # what is ready, what is blocked, and by what
jjfx dispatch --all --json    # take every ready Ticket
jjfx status --json            # workers[], waiting[], governor{}, queue[]
jjfx wait --all --until terminal --timeout 600 --json
```

Then, per finished Worker, land it (§6). Reconcile by reading `status --json`;
the local Ticket files flip to `resolved` on a **passing repository check**, not
on the agent's word - that is real and should be trusted.

**Two settings an agent cannot set today** (finding 10): the concurrency cap
(`[dispatch] max_concurrent`) and the pool size live outside the repository
(global config / `jjfx pool <N>`). Size the pool deliberately with `jjfx pool`,
and treat the cap as the user's, not yours.

---

## 3. Traps that stop a drive, and their workarounds

### 3a. Dispatching from your own opencode session fails every launch

**Fixed.** With `OPENCODE_SESSION_ID` set, jjfx used to create each Worker's
session as a **child** of yours; opencode anchors a child to *your* directory, so
the anchor assertion refused every Run (`cannot anchor Worker ...'s opencode
session: opencode anchored the Run's session to <repo root> instead of <worker
workspace>`) and no Ticket launched. Finding 01.

jjfx now asks opencode for a **root** session anchored to the Worker Workspace
whenever a child session could not be anchored there, so `dispatch` from your own
session works and needs no workaround. The child-session topology is kept only
when your session is itself anchored to the Worker Workspace; a launch that fails
the anchor check still fails closed, and now removes the session it created rather
than leaving it for `jjfx children` to report.

### 3b. Landing verbs refuse a fresh bookmark - push first

`absorb`, `rebase`, `open-pr` and `review` all resolve the Worker's bookmark
through a read that, on jj 0.46, counts an **unpushed** bookmark twice and calls
it ambiguous:

```
Error: Worker worker-XXXX's Ticket <ticket> is named by more than one bookmark
(adam/<ticket>, adam/<ticket>): keep one and delete the rest
```

The same defect makes `dispatch --all` base a dependent on trunk instead of its
Blocker's branch (`which has no branch; basing it on trunk`) when the Blocker's
branch is unpushed. Finding 02.

**Workaround:** push the Worker's bookmark by hand before landing:

```sh
jj git push -b adam/<ticket>      # clears the ambiguity
jjfx rebase worker-XXXX
```

**What jjfx should own:** not counting one bookmark twice. The verbs should work on
exactly the branch a Worker just produced.

### 3c. `jjfx review` cannot run on a current `gh`

`jjfx review` aborts with `cannot query pull-request checks: Unknown JSON field:
"conclusion"` on gh 2.102 (the field is now `bucket` / `state`). Findings 03, 04.

**Workaround:** none via `jjfx review`. Address review comments directly in the
Worker's Workspace (`jjfx mount`, or `jjfx send <worker> "<prompt>"`), or open the
PR and work it by hand.

**What jjfx should own:** reading the checks `gh` actually returns, and - if
`review` is meant to be the bounded loop its `--fg` help advertises - actually
driving `ReviewLoop` (finding 04). Until then, treat `review` as one prompt.

### 3d. A pool of N serves N Tickets

A Worker that finished a Run reads `done`, not `idle`, and is not reused. After N
Runs `dispatch --all` reports `failed` for the overflow and stderr says "Grow pool
by 1"; a named `dispatch` errors `no idle Worker is available`. Finding 07.

**Workaround:** reclaim a finished Worker before the next round:

```sh
jjfx pool reset worker-XXXX       # returns it to idle
```

or size the pool to the whole backlog up front (`jjfx pool 8`).

**What jjfx should own:** returning a Worker to the pool once its outcome is
collected, or at least having the no-capacity Dispatch name `pool reset` rather
than "grow the pool".

### 3e. `resume` can re-run landed work

A failed first attempt leaves a queue step `failed`; a later successful Dispatch
of the same Ticket does not update it. `jjfx resume` (no args) then treats the
Ticket as unfinished and **re-dispatches resolved, landed work**. Finding 06.

**Workaround:** always name the Tickets you mean:

```sh
jjfx resume sift--08-sparkline        # not `jjfx resume` bare
```

**What jjfx should own:** settling the step on the successful re-dispatch, and not
re-opening a step whose Ticket is already resolved.

---

## 4. Landing through the forge

The dispatch prompt tells the Worker jjfx will deliver (push, run the check, open
the PR) and not to push or open it itself. **No CLI verb does that** - the forge
is TUI-only, `rebase` pushes without a PR, and `open-pr` only opens an existing
one. Finding 05. Until it is fixed, the agent must bridge the gap:

```sh
jj git push --all                                   # or per bookmark (§3b)
gh pr create -R <owner>/<repo> --base main \
  --head adam/<ticket> --title "<title>" --body "<jj describe text>"
gh pr checks <n> -R <owner>/<repo>                  # wait for green
gh pr merge  <n> -R <owner>/<repo> --squash         # merge on green
```

Parallel Workers editing the same files **will conflict** at merge time - the
fixture produced three clean merges and five conflicts out of eight. Rebase the
survivors onto the new trunk and resolve with `jj resolve` / conflict markers;
`jj rebase -b adam/<ticket> -d main` shows the conflict as data.

**What jjfx should own:** the whole of §4 as one command - the "one blessed forge
path" the prompt already promises.

---

## 5. What jjfx should be doing, rather than asking the agent to remember

Consolidated from the findings. Each is a thing this drive's agent had to know,
guess, or read the source to discover:

1. **The binary.** Say which build a command needs, instead of relying on the
   agent to pin `PATH`. (§1)
2. **The dispatch session.** Anchor the Worker's session to its Workspace whatever
   dispatched it, and say which session (if any) owns it. (§3a, done)
3. **Bookmark resolution.** One bookmark is not two; landing verbs work on a fresh
   branch. (§3b)
4. **The check reader.** Read the `gh` the operator has. (§3c)
5. **The review loop.** Wire `ReviewLoop` to a command, or stop advertising it.
   (§3c, finding 04)
6. **Worker reuse.** Return a finished Worker to the pool, or name `pool reset`.
   (§3d)
7. **Resume safety.** Do not re-run a resolved Ticket. (§3e)
8. **Delivery.** Perform the push+PR the prompt promises. (§4)
9. **Capacity.** Let the repository set its own ceiling. (finding 10)
10. **Watch.** `jjfx logs` should show a session-delivered Run, or say there is no
    log. (finding 11)

An agent that has to remember ten things to land a change is being asked to be
the control plane. The point of jjfx is that it is.

---

## 6. A minimal agent checklist

- [ ] `jjfx version` is the build under test; the verbs answer.
- [ ] `jjfx ticket list` read; ready vs blocked understood.
- [ ] Pool sized; `hooks status` reported.
- [ ] `dispatch --all --json`; every Ticket has an outcome.
- [ ] `status --json` read for `workers[]`/`governor{}`/`queue[]`, not logs.
- [ ] `pool reset` a finished Worker before the next round (or pool >= backlog).
- [ ] `resume` named, never bare.
- [ ] Push bookmarks before `absorb`/`rebase`/`open-pr`.
- [ ] PR + `gh pr checks` + merge on green; expect conflicts from parallel work.
