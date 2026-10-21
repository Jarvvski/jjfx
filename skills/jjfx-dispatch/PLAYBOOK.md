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

**One setting an agent cannot set today** (finding 10, partly fixed): the pool
size lives outside the repository (`jjfx pool <N>`). Size the pool deliberately
with `jjfx pool`. The concurrency cap (`[dispatch] max_concurrent`) is now
repository-settable - put it in `<repo>/.jjfx/config.toml`, not the user's global
config, and it tightens the user-global ceiling rather than replacing it:

```toml
# <repo>/.jjfx/config.toml
[dispatch]
max_concurrent = 2
```

`jjfx status --json` reports the resolved limit as `governor.limit`, so the
binding ceiling is visible where the pool is read.

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

### 3c. `jjfx review` runs the bounded loop

**Fixed.** `jjfx review` used to abort with `cannot query pull-request checks:
Unknown JSON field: "conclusion"` on gh 2.102 (the field is now `bucket` /
`state`), and it sent one prompt that produced no findings - so the bounded loop
its `--fg` help advertised had no caller. Findings 03, 04.

`jjfx review <worker>` now drives that loop: each pass reads the PR's unresolved
review threads through `gh`, and a pass that found something sends its findings
to the Worker to address. It stops on `[review] max_passes` or on a stalled
round, and only a pass that finds nothing is success; any other stop escalates
the unresolved findings for a human and exits non-zero.

### 3d. A pool of N serves N Tickets

**Fixed.** A Worker that finished a Run reads `done`, not `idle`, and used not to
be reused: after N Runs `dispatch --all` reported `failed` for the overflow and
stderr said "Grow pool by 1", and a named `dispatch` errored `no idle Worker is
available`. Finding 07.

A finished Worker's outcome is collected once it reads `done`/`failed`, and the
next Dispatch now returns it to `idle` automatically before it looks for
capacity. A pool of N therefore serves a backlog larger than N across successive
rounds with no manual step:

```sh
jjfx dispatch --all --json   # round 1 fills the pool
jjfx wait --all --until terminal --timeout 600
jjfx dispatch --all --json   # round 2 reuses the finished Workers
```

`jjfx pool reset worker-XXXX` remains the explicit reclaim, for a Worker you want
to reset *now* rather than at the next Dispatch.

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
the PR) and not to push or open it itself. That is `jjfx deliver` - the one forge
path, and the command the prompt and the skill now name:

```sh
jjfx deliver worker-XXXX      # weld, check, push, open one PR
jjfx deliver adam/<ticket>    # the same delivery, named by the change's bookmark
```

It welds the Worker's chain onto trunk, runs the repository's own check
(`<repo>/.jjfx/config.toml` `[checks]`), pushes the chain, and opens one Pull
Request per change based on the repository's default branch. A failed check, a
conflict, or an unreachable forge stops the run before the push, names why and
exits non-zero; a change that already has a PR is left alone, so re-running is
safe. A repository with no forge (`[forge] pull_requests = false`, or no `origin`
remote to open one against) is still delivered - the PR step does not apply, and
a clean weld, check and push succeeds. **Do not run `gh pr create` yourself**:
that is `deliver`. `rebase` pushes without a PR and `open-pr` only opens an
existing one, so neither is the delivery on its own.

Parallel Workers editing the same files **will conflict** at merge time - the
fixture produced three clean merges and five conflicts out of eight. Rebase the
survivors onto the new trunk and resolve with `jj resolve` / conflict markers;
`jj rebase -b adam/<ticket> -d main` shows the conflict as data.

**Re-weld by bookmark, not by Worker.** The moment the first PR merges, trunk
moves under the others and their Pull Requests read `CONFLICTING` - and by then
the Worker that delivered one may already have been reused for the next Ticket,
which is `dogfood-2` finding 07 working as designed. A Worker is a reusable slot;
**the bookmark is the durable name of the change**, so `rebase`, `deliver` and
`open-pr` take either:

```sh
jjfx rebase adam/tally--04-json-output    # re-weld the delivered change by name
jjfx deliver adam/tally--04-json-output   # the whole path, check included
```

When no Workspace holds the change, jjfx creates one for it (`branch-<bookmark>`)
and says so. A conflicted rebase is left in place and names that Workspace, so the
loop is:

```sh
jjfx rebase adam/tally--04-json-output            # names the Workspace on conflict
jjfx resolve branch-adam-tally--04-json-output    # the merge tool runs there
jjfx rebase adam/tally--04-json-output            # pushes the resolved change
```

**What jjfx should own:** this used to be "fetch, find the branch, make a
workspace, rebase it, resolve, push" by hand. **Done:** naming the bookmark does
all of it. Finding 02.

The merge itself is still the loop's: `gh pr checks <n> -R <owner>/<repo>` then
`gh pr merge <n> -R <owner>/<repo> --squash` once it is green. jjfx has no merge
step (10-opinionated-landing).

**What jjfx should own:** the whole of this as one command - the "one blessed
forge path" the prompt already promises. **Done:** `jjfx deliver`.

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
5. **The review loop.** Wire `ReviewLoop` to a command. (§3c, finding 04, done:
   `jjfx review`)
6. **Worker reuse.** Return a finished Worker to the pool. (§3d, done)
7. **Resume safety.** Do not re-run a resolved Ticket. (§3e)
8. **Delivery.** Perform the push+PR the prompt promises. (§4, done: `jjfx deliver`)
9. **Capacity.** Let the repository set its own ceiling. (finding 10, done for
   the concurrency cap; the pool size is still `jjfx pool`.)
10. **Watch.** `jjfx logs` shows a session-delivered Run's activity, or says there
    is no log. (finding 11, done)

An agent that has to remember ten things to land a change is being asked to be
the control plane. The point of jjfx is that it is.

---

## 6. A minimal agent checklist

- [ ] `jjfx version` is the build under test; the verbs answer.
- [ ] `jjfx ticket list` read; ready vs blocked understood.
- [ ] Pool sized; `hooks status` reported.
- [ ] `dispatch --all --json`; every Ticket has an outcome.
- [ ] `status --json` read for `workers[]`/`governor{}`/`queue[]`, not logs.
- [ ] Next round after `wait --until terminal`; a finished Worker is reused automatically.
- [ ] `resume` named, never bare.
- [ ] `jjfx deliver worker-XXXX` to land: weld, check, push, one PR (no `gh pr create`).
- [ ] A PR that conflicts after a sibling merged: re-weld it by bookmark (`jjfx rebase adam/<ticket>`), resolve in the Workspace it names, re-run.
- [ ] `gh pr checks` + merge on green; expect conflicts from parallel work.
