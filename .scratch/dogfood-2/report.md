# The second dogfood drive: does jjfx make a Ticket backlog better?

A first-hand report from driving the `sift` backlog (eight Tickets, a real pytest
suite, real GitHub CI) through jjfx. Every claim carries the command that produced
it and the output it produced. The friction is filed as Tickets under
`.scratch/dogfood-2/issues/`, each on its own PR; this report is separate from
that backlog, as the first drive's was.

Binary under test: `/Users/jarvis/Code/personal/jjfx/target/release/jjfx`
(`jjfx 0.55.0-dev`), pinned first on `PATH` and verified (`jjfx version`, and
`help dispatch` / `help resume` / `help ticket list` / `children` / `absorb` all
answering). `jj 0.46.0`, `gh 2.102.0`, opencode, Python 3.12 fixture.

Fixture: `/Users/jarvis/Code/personal/jjfx-drive-2` - eight Tickets, six ready and
two blocked on a `01 -> 07 -> 08` chain, four touching `sift/cli.py` /
`sift/aggregate.py` / `sift/format.py`, one (05) a design decision. `origin` is a
real throwaway GitHub repository with a CI workflow. Left behind deliberately: the
branches, the PRs, the conflicted rebases, and the fixture's `.jj` state.

---

## 0. The shape of the drive

Driven the way an agent would: `jjfx ticket list` to choose work, a four-Worker
pool under a concurrency cap of two, `dispatch --all`, `wait`, `status --json`,
`children`, `resume`, reconcile, and the forge path (push, PR, `gh pr checks`,
merge on green). The review step was included. The orchestrator was this agent;
jjfx spawned every Worker.

**Result: all eight Tickets resolved and landed.** All eight are PRs in the
fixture - three merged cleanly, five left open on a merge conflict (§3.8); the
work is real (`runs.jsonl` records `check: passed` for every one, and CI is green
on all eight PRs). The landing half was done by hand, because
jjfx's landing verbs could not do it (finding 02) - see §3.

---

## 1. What worked, and should not be refactored away

- **The concurrency governor holds, across dispatches.** With
  `dispatch.max_concurrent = 2`, a four-Worker pool and six ready Tickets, the
  first `dispatch --all` fired two and held four; a *second* `dispatch --all`
  while those two ran held its whole ready set, naming the reason:
  `held back by the concurrency cap (dispatch.max_concurrent = 2): 2 Run(s) in
  flight`, and `status --json` reported `governor: {"limit":2,"running":2,"held":4}`.
  The "told 3, ran 6" failure does not reproduce. (`harness/05`)
- **`resume` recovers interrupted work.** A Run was interrupted mid-flight
  (`opencode api session.interrupt`); the Worker settled `failed` with
  `OpenCode session interrupted`, the queue recorded it, and `jjfx resume
  sift--08-sparkline` re-dispatched it with
  `detail: "resumed after failed: OpenCode session interrupted"` - and the Ticket
  then resolved. (`harness/07`)
- **The durable queue survives and is complete.** Before a Run's launch the queue
  holds an `in_flight` record naming the Worker, Workspace, branch, runtime and
  session; it is read back by fresh processes, and `status --json` carries it with
  stable field names. (`harness/06`)
- **Verified done is real.** A Ticket resolved only when the repository's
  `[checks]` command ran and passed: `runs.jsonl` carries `"check": "passed"` for
  each success, and the local Ticket files were rewritten to `Status: resolved`.
  (`harness/02`)
- **`absorb` no longer rewrites trunk.** On a Worker whose change was already
  committed and whose branch was pushed: `Nothing to absorb for
  adam/sift--08-sparkline: no working-copy edit belongs in an earlier commit`,
  and `main`'s commit id was unchanged before and after. (`dogfood/01` holds.)
- **The forge slug is derived correctly.** `pool.json` records
  `"gh_repo": "Jarvvski/jjfx-drive-2"` from the real GitHub remote - no fabricated
  slug. (`dogfood/03`, `10`, `11` hold.)
- **Dependency stacking works.** Once a Blocker's branch was pushed, the dependent
  was materialised as a real stacked workspace: `stack-bases.json` recorded the
  Blocker's commit (`78874ee`) as the dependent's base, and `jj log -r 'main..@'`
  in the dependent's Workspace showed the Blocker's commits below its own.
- **`dispatch --all` names the blocked Ticket on stderr, out of stdout.**
  `sift--08-sparkline: blocked by sift--07-auto-buckets (ready-for-agent)`, with
  `--json` still parsing. (`dogfood` positive, unchanged.)
- **`pool profile` before the pool exists now works** (`dogfood/04` fixed):
  `jjfx pool profile opencode` on a repository with no pool printed
  `Pool profile set to opencode`, exit 0.
- **`children` reaches opencode even with no lifecycle plugin.** `jjfx children
  --json` listed the child sessions opencode knows, `source: "opencode"`, each
  with its title and directory; their `state` was `absent` because the plugin is
  not installed.

---

## 2. Falsification attempts and their outcomes

| Claim to falsify | Attempt | Outcome |
|---|---|---|
| Capacity governor holds under load (`harness/05`) | cap 2, pool 4, 6 ready, then a second dispatch while 2 ran | **Held.** `running:2, held:4`; the second batch held its whole ready set. |
| `resume` recovers in-flight work (`harness/07`) | interrupted a session mid-Run, then `resume` | **Recovered.** `resumed after failed: OpenCode session interrupted`. |
| The durable queue survives a restart (`harness/06`) | read `queue.json` / `status --json` from fresh processes | **Survived**, and complete. But it can go **stale** (finding 06). |
| The review bound stops a loop that would spin (`harness/04`) | `jjfx review --fg`, which help says runs "the full bounded review loop" | **Could not test - no command invokes the loop.** `review` never reaches `ReviewLoop` (finding 04). |
| A repeated review dimension does not reach the model (`harness/09`/`12`) | drive a review twice | **Not reachable from the CLI.** The loop has no caller, so no dimension ever runs to repeat. |
| `resolved` means verified, not the agent's word (`harness/02`) | inspect run records against resolved Tickets | **Held.** `check: passed` gates resolution. |
| `absorb` cannot rewrite trunk (`dogfood/01`) | absorb a committed, pushed Worker | **Held.** No-op; `main` unchanged. |
| The forge slug is real (`dogfood/03`/`10`/`11`) | read `pool.json` | **Held.** `Jarvvski/jjfx-drive-2`. |
| An agent can dispatch from its own session (`harness/08`) | dispatch with `OPENCODE_SESSION_ID` set | **Failed.** Every launch failed the anchor assertion (finding 01). |
| An agent can land through jjfx's landing verbs | `absorb` / `rebase` / `open-pr` on a finished Worker | **Failed.** All refused on a fresh bookmark (finding 02). |
| `jjfx review` addresses a PR's review | `jjfx review` on a Worker with a green PR | **Failed.** `gh` field mismatch (finding 03). |

**Not settled:** whether the review bound *would* terminate a spinning loop. The
mechanism is tested at the harness seam (`harness/04`/`12`), but no operator
surface reaches it, so I could not exercise it against a real PR. The honest
answer to the map's question is therefore: **an agent working a backlog through
jjfx never repeats a review dimension, because it never runs one - and nothing
notices, because there is nothing to notice.**

---

## 3. What actually happened, in order

1. **Setup.** `pool profile opencode` (before the pool - works), `pool 4`,
   `status --json` (governor present, `limit: null`). `hooks status` reported
   `opencode: missing: lifecycle plugin` (not installed; the skill says install
   only when the user asks, so it was reported, not fixed).
2. **The wall: no Dispatch from an agent's session.** `dispatch --all` failed
   every launch with the anchor error (finding 01). Workaround discovered by
   reading the source: `env -u OPENCODE_SESSION_ID jjfx dispatch ...` - which
   gives up the child-session topology `harness/08` was built for. The rest of the
   drive ran this way. This is *"the agent had to know X"* in its purest form.
3. **Governor + first Run.** Under the cap, one Run ran and finished in 4m30s,
   producing the Ticket's two commits; `check: passed`; the Ticket resolved. Two
   Tickets were dispatched per round thereafter.
4. **Landing.** `absorb` / `rebase` / `open-pr` all refused the fresh bookmark
   (finding 02). Workaround: `jj git push -b <branch>` by hand first, after which
   `rebase` worked. The dispatch prompt promises jjfx pushes and opens the PR; no
   CLI verb does (finding 05), so the PRs were opened with `gh pr create` - the
   step the prompt tells the agent *not* to do.
5. **Review.** `jjfx review --fg` aborted on the `gh` field mismatch (finding 03).
   Reading the source showed that even when it runs, it sends one prompt and never
   drives the bounded loop (finding 04).
6. **Reclaim.** After four Runs the pool had no idle Worker and `dispatch`
   reported `failed` / hard-errored; `pool reset` was needed to continue (finding
   07).
7. **Resume + the stale queue.** Interrupting a Run and resuming it worked. But
   `resume` with no arguments re-dispatched **already-resolved, already-landed**
   Tickets 01 and 02, because a failed first attempt left their queue steps
   `failed` and a later successful dispatch never updated them (finding 06).
8. **Forge.** Eight branches pushed, eight PRs opened, all eight green on CI.
   Merging sequentially: **three merged cleanly, five conflicted** - the
   same-file parallel edits produced real integration conflicts, exactly as the
   fixture intended. Rebasing one conflicted branch onto the new `main` produced
   conflicts in `aggregate.py`, `cli.py`, `test_aggregate.py` and `test_cli.py`
   (conflict-as-data, working as designed).

---

## 4. Where the harness batch moved the needle, and where it did not

**It moved it, on the dispatch half.** Governor, durable queue, verified-done and
resume are all real and all held under a real backlog: the capacity governor is
the clearest win (the "told 3, ran 6" failure is gone), `resume` genuinely
recovers an interrupted Run, and a Ticket no longer resolves on the agent's word.
The dispatch half of jjfx is now better than hand-running.

**It did not move it, on the loop and the landing half.** The batch's headline
reductions are unreachable from the operator surface:

- the **review bound** (`harness/04`) and the **decision-log replay**
  (`harness/12`) have no caller, so the recurring-dimension pain is untouched for
  an agent driving jjfx (finding 04);
- the **one blessed forge path** (`harness/10`) is TUI-only; the CLI cannot
  deliver what the prompt promises (finding 05);
- and the **child-session topology** (`harness/08`) actively *breaks* the
  agent-driven dispatch path it was meant to improve (finding 01).

So the pain did not go away; it moved. The first drive's landing-half pain
(bookmark resolution, trunk, slug) is largely fixed, and a new landing-half pain
took its place: the bookmark read (finding 02) and the missing CLI delivery
(finding 05). The loop-half pain (a review loop that spins, dimensions that
recur) is not addressed at all, because the loop is not wired to anything an
operator can call.

**Judgement.** jjfx is now a good *dispatch* control plane and a broken *landing*
one, and the review loop it was built to bound is not reachable. For an agent
working a backlog, the honest experience was: `ticket list` and the governor made
choosing and pacing work genuinely better, `resume` and verified-done made
recovery trustworthy, and then **every command in the landing and review half
failed**, each for a reason the agent had to read the source to find. The
harness batch moved the dispatch half a long way and the landing/loop half not at
all.

---

## 5. Friction, filed as Tickets

Each is a file under `.scratch/dogfood-2/issues/` on its own branch and PR.

| # | Finding | Kind | PR |
|---|---|---|---|
| 01 | Dispatching from inside an opencode session cannot launch a Worker (child session anchored to the parent's directory); the failed launch leaks the session | bug | #146 |
| 02 | Landing verbs refuse a fresh bookmark: `jj bookmark list -T name` counts an unpushed bookmark twice, so it reads as ambiguous and blocks `absorb`/`rebase`/`open-pr`/`review`, and silently degrades dependency stacking | bug | #147 |
| 03 | `jjfx review` cannot parse `gh pr checks --json` on gh 2.102 (no `conclusion` field) | bug | #148 |
| 04 | The bounded review loop has no caller, and `jjfx review --fg`'s help claims it runs it | design | #149 |
| 05 | No CLI verb performs the push+PR delivery the dispatch prompt promises (forge is TUI-only) | bug | #150 |
| 06 | A re-dispatch that succeeds leaves the durable queue reporting the failure; `resume` then re-runs landed work | bug | #151 |
| 07 | A finished Worker is never returned to the pool, so a pool of N serves N Tickets; `dispatch` says "grow the pool" instead of "reset a Worker" | bug | #152 |
| 08 | `dispatch --all` reports a capacity gap as `failed`, and the cap detail says "0 Run(s) in flight" when the cap is what bound it | bug | #153 |
| 09 | `jjfx help ticket` is an unknown command | bug | #154 |
| 10 | The concurrency cap can only be set user-globally (no repo-level or CLI override) | feature | #155 |
| 11 | `jjfx logs` follows an empty file for a session-delivered Run | bug | #156 |
| 12 | `rebase` mutates the Workspace and then fails, leaving conflicts behind | bug | #157 |

---

## 5a. Triage of the twelve findings

All twelve were filed as findings and carried `needs-triage`. They are now
`ready-for-agent`: the evidence for each is a **live reproduction during the
drive**, and their acceptance criteria were already specific and testable.

Independently re-verified after the drive (by the orchestrator, against the
merged source and the locally built binary):

| Finding | Independently verified? |
|---|---|
| 02 landing verbs refuse a fresh bookmark | **confirmed** - `jj bookmark list -T name` printed the same bookmark twice |
| 03 `review` cannot parse `gh pr checks` | **confirmed** - `gh 2.102.0` rejects `--json conclusion` |
| 04 the bounded loop has no caller | **confirmed** - `help review` claims it; nothing reaches it |
| 05 no CLI verb delivers the push+PR | **confirmed** - no verb opens a Pull Request |
| 09 `help ticket` is unknown | **confirmed** |
| 10 the cap is user-global only | **confirmed** - a repository may set only `[checks]`, `[pools]`, `[tickets]` |
| 01 dispatch from an opencode session | mechanism checked (`OPENCODE_SESSION_ID` is set here; the anchor is asserted); the failing launch was not re-run, because it spends a Run |
| 06, 07, 08, 11, 12 | not re-verified; the drive reproduced each live |

**One item from the playbook has no finding behind it**: *"say which build a
command needs"*. That is already covered by `dogfood/07` (#140), which added the
pin-the-binary paragraph to the skill, so it is not filed again.

---

## 6. The usage playbook

`skills/jjfx-dispatch/PLAYBOOK.md` (new, beside `SKILL.md`) is the agent-facing
guide this drive produced: the drive-tested loop, the traps and their
workarounds, and - the point the owner cares about - the list of things jjfx
should be doing for the agent rather than asking it to remember. `SKILL.md` gains
a pointer to it.

---

## 7. Method notes

- The concurrency cap is user-global only (finding 10); to exercise the governor
  the user's `~/.config/jjfx/config.toml` was edited to add
  `[dispatch] max_concurrent = 2` and restored afterwards. No other change was
  made outside the fixture and the jjfx repository's `.scratch/` and `skills/`.
- The fixture's Tickets were implemented by jjfx's Workers, not by hand. The
  orchestrator never edited `sift/`; the only hand-work was the landing (push, PR,
  merge) and one conflict-rebase, both because jjfx's own verbs could not do them.
- Every command in this report was run with the local build first on `PATH`, and
  `jjfx version` was re-checked after any `PATH` change.
