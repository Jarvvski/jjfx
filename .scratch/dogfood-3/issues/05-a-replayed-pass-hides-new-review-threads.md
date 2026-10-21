# A recorded review pass replays forever, so a new review thread is invisible and the loop reports convergence

Status: resolved
Type: bug
Blocked by: none

## Problem

A pass is keyed by `review:<ticket>:<dimension>:<pass>`, and a recorded pass is
replayed rather than re-read. Nothing in the key changes when the Pull Request's
review state changes, so **the first decision a pass makes is permanent**: a
review thread opened afterwards is never seen, and the loop reports a clean
convergence on a Pull Request that has an unresolved thread.

Reproduced on `Jarvvski/jjfx-drive-3#9` (`tally--09-json-array-input`), whose PR
is open with a Worker holding its branch:

```
$ jjfx review worker-348336 --fg          # the PR has no review threads yet
Reviewing worker-348336: Jarvvski/jjfx-drive-3#9 (at most 2 pass(es))
Review of worker-348336: 1 pass(es), 0 finding(s), stopped on convergence (a pass found nothing)

$ cat .jj/pools/default/decisions.json
{"decisions": {"review:tally--09-json-array-input:threads:1":
  {"findings": [], "completeness": "complete"}}}
```

A reviewer then opens a thread on the PR - one genuinely unresolved conversation:

```
$ gh api graphql -f query='query { repository(owner: "Jarvvski", name:
  "jjfx-drive-3") { pullRequest(number: 9) { reviewThreads(first: 20) {
  nodes { isResolved path } } } } }' \
  --jq '[.data.repository.pullRequest.reviewThreads.nodes[] | select(.isResolved==false)] | length'
1
```

and the next review **does not read it**:

```
$ jjfx review worker-348336 --fg
Reviewing worker-348336: Jarvvski/jjfx-drive-3#9 (at most 2 pass(es))
Review of worker-348336: 1 pass(es), 0 finding(s), stopped on convergence (a pass found nothing)
  1 pass(es) replayed from the decision log
```

`convergence (a pass found nothing)` is false. The pass found nothing because it
never ran: it was served from a decision recorded when the PR had no threads, and
the thread is still open on the PR. The Worker is never told, and the operator is
told the review is clean.

Replay is right within one loop - a pass that already ran must not be re-run to
buy the same findings - and the second drive's `harness/09`/`harness/12` built it
for exactly that. What it does not survive is the *next* invocation: a review
requested after new feedback arrives is the case the command exists for, and it
is the one case replay defeats.

## Acceptance Criteria

- [x] A recorded pass must not outlive the state it decided on. A review thread
  opened after a pass was recorded is visible to a later `jjfx review`, and that
  review reads it.
- [x] **`jjfx review` never reports convergence while the Pull Request has an
  unresolved review thread** - the loop's own honesty rule, and the thing this
  bug breaks. If it cannot tell, it must say so rather than claim a clean pass.
- [x] Whatever shape is chosen - a key that includes the review state, a pass that
  is only replayed within a single invocation, or an explicit refresh - the
  bound is unchanged: `max_passes` and the progress rule still stop the loop, and
  a loop that stops on the cap or a stall still escalates.
- [x] A test holds it: a recorded converged pass plus a new unresolved thread must
  not converge on the replay alone.

## Resolution

**The shape: replay is scoped to a single invocation.** `jjfx review` drives its
loop over a decision log that is created fresh for the run and forgotten when the
run ends, rather than over the pool's durable sidecar. A pass already decided
*within* a run still replays; a pass decided by an *earlier* `jjfx review` is
never replayed, so every invocation reads the Pull Request's review threads as
they are now.

Why this shape and not the other two:

- **A key that includes the review state** would have to read the threads to
  compute the state, which is exactly the `gh` read replay exists to skip - and no
  cheap PR token captures "a thread was opened": a `COMMENTED` review adds a thread
  without moving `reviewDecision`, and a thread can be opened with no push, so the
  head commit does not move either. The only reliable state is the thread set
  itself, which means reading it.
- **An explicit refresh** cannot hold the rule on its own: the default command
  would still report convergence on a replayed pass, so an operator who forgot the
  flag would still be lied to. The rule has to hold by default.

Replay is genuinely right *within* a loop, so the seam is kept, not deleted: the
harness still replays across invocations over the durable sidecar
(`RecordedReviewLoop::new`), because its reviewer is a model whose decisions do
not depend on state the outside world can move. Only the binary's driver changed
(`RecordedReviewLoop::per_invocation`).

**The bound is unchanged.** `ReviewLoop` and its `drive` are untouched: the pass
cap and the progress rule stop the loop exactly as before, a replayed pass still
counts as a round, and a loop that stops on the cap or a stall still escalates and
exits non-zero. The escalation output on the reproduction now names the open
thread instead of claiming convergence.

What changed:

- `src/engine/review_run.rs` - `EphemeralDecisionLog` (an in-memory `DecisionLog`
  scoped to one run) and `RecordedReviewLoop::per_invocation`, which builds the
  driver over it. The durable `RecordedReviewLoop::new` is unchanged for the
  harness.
- `src/cli.rs` - `review_command` uses `RecordedReviewLoop::per_invocation`, so it
  no longer reads or writes the pool's decision sidecar for review passes. Passes
  are still recorded in the durable queue for `status`.
- `tests/review_contract.rs` - the replay test (which asserted the buggy "no
  second `gh` read") is replaced by
  `a_thread_opened_after_a_converged_pass_is_read_by_the_next_review`: a converged
  first drive, then a thread opened with no push, then a second drive that must
  read it and must not report convergence. Reverting the wiring to the durable log
  makes the test fail with exactly the reported output, so it holds the rule.
- `src/engine/review_run.rs` unit test -
  `an_invocation_scoped_driver_reaches_the_reviewer_on_every_run` proves two
  drivers over the same Repository each reach the reviewer.
- Docs - `docs/review-loop.md` and `docs/work-queue.md` now state that `jjfx
  review` keeps replay to one invocation and why a decision must not outlive the
  threads it saw; `momo fix` records the user-visible change.

