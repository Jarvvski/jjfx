# The third dogfood drive: does jjfx own the loop, and does it hold on a clean config?

A first-hand report from driving the `tally` backlog (eight Tickets, a real
`unittest` suite, real GitHub CI) through jjfx. Every claim carries the command
that produced it and the output it produced. The friction is filed as Tickets
under `.scratch/dogfood-3/issues/`; this report is separate from that backlog, as
both earlier drives' were.

Binary under test: `/Users/jarvis/Code/personal/jjfx/target/release/jjfx`
(`jjfx 0.55.0-dev`), built from `main` after the second drive's twelve findings
landed, pinned first on `PATH` and verified. `jj 0.46.0`, `gh`, opencode, Python
3.15 fixture.

Fixture: `/Users/jarvis/Code/personal/jjfx-drive-3` - `tally`, a Python CLI that
summarises JSONL log records, 41 tests to begin with. Eight Tickets, six ready
and two blocked on a `01 -> 07 -> 08` chain, four touching `cli.py` /
`aggregate.py` / `format.py`, one (05) a design decision, one (06) a bug that
reproduces from a plain file. `origin` is a real private throwaway repository,
`Jarvvski/jjfx-drive-3`, with GitHub Actions CI.

---

## 0. The shape of the drive

Driven the way an agent would: `jjfx ticket list` to choose work, a four-Worker
pool under a concurrency cap of two, `dispatch --all`, `status --json`,
`children`, `logs`, `deliver`, and the forge path (push, PR, `gh pr checks`,
merge on green). The orchestrator was this agent; jjfx spawned every Worker.

**Result: all eight Tickets resolved and verified, and seven of the eight
landed.** `runs.jsonl` records `check: passed` for every one, the fixture's CI is
green on every PR, and the merged trunk's suite grew from 41 tests to 111 with
881 insertions across 16 files. The eighth PR is open on a merge conflict - see
§3.2 - and jjfx offers no way to resolve it.

---

## 1. What worked, and should not be refactored away

- **`jjfx deliver` performs the whole delivery.** The second drive's headline
  gap (finding 05: no CLI verb does what the prompt promises) is closed, and the
  drive is the falsification attempt that failed to break it. On a finished
  Worker: `jjfx deliver worker-348336` → `Delivering worker-348336: weld, check,
  push, Pull Request` / `Delivered worker-348336`, and the repository then has a
  real PR whose CI is green. Seven times, in five dispatch rounds, with no
  hand-run `jj push` anywhere in the drive.
- **The concurrency governor holds, across dispatches.** With
  `[dispatch] max_concurrent = 2` in the repository's own config - the
  repository-local ceiling `dogfood-2`'s finding 10 added - a four-Worker pool
  and six ready Tickets: the first `dispatch --all` fired two and held four
  (`held back by the concurrency cap (dispatch.max_concurrent = 2): 2 Run(s) in
  flight`), and `status --json` reported `governor: {"limit":2,"running":2,
  "held":4}`. Every later batch held its excess the same way.
- **Verified done is real, and the check gates resolution.** All eight Runs
  record `check: passed`, and the Ticket file is only rewritten to
  `Status: resolved` after that. Nothing in the drive resolved a Ticket on an
  agent's word.
- **A finished Worker is returned to the pool.** `dogfood-2`'s finding 07 is
  fixed and visible: five Tickets were served by `worker-348336` and three by
  `worker-a04066` - two Workers carried eight Tickets, and the other two pool
  members were never needed. Each re-dispatch reused the idle Worker rather than
  reporting "grow the pool".
- **Dependency readiness readies a dependent in the same Dispatch.** `tally--07`
  was blocked on `tally--01` and read `blocked by tally--01-percentile-durations
  (ready-for-agent)`; the moment 01 resolved, the next `ticket list` showed
  `tally--07-sparkline-per-group  ready-for-agent  ready`, with no reconcile step
  by hand.
- **`jjfx logs` follows a session-delivered Run.** `dogfood-2`'s finding 11 is
  fixed: `jjfx logs worker-348336` printed the session's own activity - its
  reasoning, its `tool shell Completed python3 -m unittest discover` lines, the
  sample JSONL it was testing with - rather than following an empty file.
- **`children` reaches opencode with no lifecycle plugin.** `jjfx children
  --json` listed 50 child sessions with their titles and directories, `state:
  "absent"` because the plugin is not installed, which is what `dogfood-2`
  reported and what `dogfood-2`'s finding 01 left fixed.
- **A repository cannot set `[forge]`, and the refusal is loud and correct.**
  A `[forge]` section in `.jjfx/config.toml` is rejected at load, naming the
  file, the line and the permitted sections. The rule is the one ADR 0014
  states - see §3.1 for why the *consequence* is still a finding.

---

## 2. Falsification attempts and their outcomes

| Claim to falsify | Attempt | Outcome |
|---|---|---|
| `deliver` really welds, checks, pushes and opens a PR | `jjfx deliver` on five batches, then `gh pr list` | **Held.** Seven PRs, each with the Worker's description, each with a green check, each mergeable from a fresh trunk. |
| The repository-local cap binds | `[dispatch] max_concurrent = 2` in `.jjfx/config.toml`, six ready | **Held.** `governor.limit` 2, four held, repeated across batches. |
| A resolved Ticket means the check passed | `runs.jsonl` against every resolved Ticket | **Held.** Eight Runs, eight `check: passed`, no Ticket resolved without one. |
| A landed PR can be merged on green | `gh pr merge` on each PR after `gh pr checks` | **Failed.** Every PR is a draft, and `gh pr merge` refuses one. Finding 01. |
| A conflicting PR can be fixed through jjfx | `jjfx deliver` / `rebase` / `absorb` on the delivering Worker | **Failed.** The Worker had been reused, and every verb takes a Worker. Finding 02. |
| An opencode Run's cost is recorded | `jjfx status` after eight Runs | **Failed.** `8 Run(s) (8 without a reported cost)`, so the `[spend]` cap cannot bind. Finding 04. |
| The review loop can be exercised | `jjfx review` against a PR | **Could not test.** No command drives `ReviewLoop` - the owner's ruling (finding 03) is the answer, and its implementation is in flight. |
| A Worker can run `jjfx` itself | the prompt names `jjfx deliver <worker>`; a login shell resolves `jjfx` | **Held, by luck.** A Worker's login shell resolves `jjfx` to `~/.cargo/bin/jjfx` (a released binary), not the build under test. No Worker needed it, because the prompt also tells them not to push - but the trap is live. |

---

## 3. Where it broke

### 3.1 Every delivered PR is a draft, and a repository cannot say otherwise

`deliver` opens the PR as a draft (`[forge] draft` defaults to true, and
`[forge]` is user-global only), so the landing flow it exists for stalls at the
last step: `gh pr merge 1` → `GraphQL: Pull Request is still a draft
(mergePullRequest)`, with a green check and no jjfx verb that marks a PR ready.
`gh pr ready` fixes it and nothing in jjfx's help, docs or prompt says so. Filed
as **finding 01**.

### 3.2 A delivered PR cannot be re-welded, and reuse destroys the only handle

Two Workers delivered from the same trunk in one batch; the second PR conflicted
as soon as the first merged. Every landing verb takes a `<worker>` and nothing
else, and by then the Worker that delivered the conflicting PR had been **reused
for the next Ticket** - finding 07's fix working as designed. `jjfx deliver
worker-a04066` now delivers a different Ticket, and no jjfx verb names
`adam/tally--04-json-output`. The change is reachable only through `jj` by hand.
Filed as **finding 02**; PR 4 is left open as its evidence, exactly as the second
drive left five.

### 3.3 An opencode Run records no cost

`jjfx status` after eight Runs: `8 Run(s) (8 without a reported cost)`. The
`[spend]` cap is enforced from those records, so for an opencode pool the cap is
inert - `cap_usd` cannot bind, and `on_exceed` can never fire. Filed as
**finding 04**.

### 3.4 The review loop still has no caller

Unchanged from the second drive, and now answered rather than left open: the
owner has ruled that jjfx owns the loop with the PR's review threads as its
findings source. Filed as **finding 03**, with the implementation in flight.

---

## 4. The landing half, judged

The second drive's judgement was that jjfx was "a good dispatch control plane and
a broken landing one". **That has changed.** An agent can now finish a Worker and
land it with one jjfx command, and the drive did it seven times without reaching
for `jj push` or `gh pr create`. What remains broken is *narrower and later*: not
the delivery, but what happens when two deliveries race, and when the PR a
delivery opened is not in the state the agent assumed.

The pattern is the same one both earlier drives found: **jjfx's model of a
delivered change ends at the push.** It knows a Worker, a Workspace, a branch and
a Run; it does not know a Pull Request as an object it can go back to. The
draft state (01) and the conflict (02) are two faces of that - both are facts
about the PR that no jjfx verb can act on, and both surfaced only after
`deliver` reported success.

---

## 5. Friction, filed as Tickets

| # | Finding | Type |
|---|---|---|
| 01 | Every delivered PR is a draft and a repository cannot choose otherwise, so the landing flow stalls at `gh pr merge` | bug |
| 02 | A delivered PR whose trunk moved cannot be re-welded, and reusing its Worker destroys the only handle to it | bug |
| 03 | jjfx owns the bounded review loop again, sourced from the PR's review threads (the owner's ruling) | task |
| 04 | An opencode Run records no cost, so the `[spend]` cap is inert | bug |

## 6. Evidence left behind

- `~/Code/personal/jjfx-drive-3` - the fixture, its branches, its PRs, its
  `runs.jsonl`, its `.jj` state.
- `Jarvvski/jjfx-drive-3` - the real private repository: seven merged PRs, one
  open on a conflict, CI green on `main`.
- The fixture's merged trunk: 111 tests passing, 881 insertions across 16 files.
