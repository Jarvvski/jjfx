# Drive the review loop through the decision log, so a repeat replays

Status: resolved
Type: task
Blocked by: none

## Problem

`harness/09` gave a review step an identity
(`review:<ticket>:<dimension>:<pass>`) and built replay: a recorded decision
lives in `.jj/pools/<name>/decisions.json`, and `ReplayingReviewer` returns it
**without calling the reviewer behind it** - proven by a test that replays
through a reviewer which panics if asked.

It is a mechanism with tests and **no caller**. Nothing in the binary drives a
review pass through the decision log, so in practice the seven review dimensions
still recur 8-11x and each recurrence still reaches the model. The repeat is
recorded and detectable; it is not yet avoided.

`09` disclosed this plainly rather than hiding it behind its ticked boxes, and
that disclosure is the reason this ticket exists.

## Acceptance Criteria

- [x] A review pass is driven through the decision log, so a step already
      recorded replays rather than invoking the reviewer.
- [x] The recurrence the parent ticket was filed for is measurably reduced: a
      repeated (ticket, dimension, pass) does not reach the model.
- [x] An operator can see that a pass was replayed rather than run.

## References

- `.scratch/harness/issues/09-idempotent-steps.md`
- `src/engine/reviewer.rs` (`ReplayingReviewer`, `DecisionLog`)
- `docs/work-queue.md`

## Resolution

`09` left a mechanism with no caller; this wires the caller.

**The loop drives every pass through the log (criterion 1).**
`ReviewLoop::run_recorded` keys each pass by `StepKey::review(ticket, dimension,
pass)` and serves it through `ReplayingReviewer`, so a pass the log already
recorded replays instead of reaching the reviewer. `ReviewLoop::run` and
`run_recorded` share one `drive`, so the cap and the progress rule are the same
code for both. `RecordedReviewLoop` (`src/engine/review_run.rs`) is the driver
that owns the log, the subject and the queue.

**The repeat does not reach the model (criterion 2).** A loop driven twice
replays every pass the second time. The proof is the panicking reviewer `09`
built: `the_loop_replays_a_recorded_pass_without_reaching_the_reviewer` and
`a_replayed_pass_still_counts_against_the_pass_cap` drive a first loop through a
scripted reviewer, then a second through a reviewer that panics if asked - the
second drive completes with every pass marked replayed, so no pass reached the
model. `tests/idempotent_steps.rs` drives the same thing end to end from a fresh
`Repository` handle, so the replay is durable rather than this process's memory.

**An operator sees a replay (criterion 3).** A replayed step stays `done` and its
durable queue item's `detail` is set to `replayed from the recorded decision`, so
`jjfx status --json` distinguishes it from a pass that ran and found nothing:

```bash
jjfx status --json | jq '.queue[] | select(.step | startswith("review:"))'
```

`WorkQueue::record_replay` annotates the settled record rather than moving it -
a replay is the same work, already done - and a step the queue does not hold is
not invented. `ReviewLoopSummary` also reports `replayed()` and
`replayed_passes()`, so a harness reads which passes replayed rather than
inferring it.

**How replay composes with `04`'s bound.** Settled, and recorded here rather than
papered over: **a replayed pass is a round that happened.** It carries the
decision it recorded, so the pass cap and the progress rule see exactly the
rounds the first drive saw and the loop stops in the same place - which is what
makes a second drive reach no model instead of buying a fresh, unrecorded pass
past where the first stopped. What replay removes is the reviewer call *and* the
re-address: a replayed pass's findings were addressed when the decision was first
made, so `address` is not called for it. The bound's code is unchanged; the
tests that pinned `04`'s behaviour still pass.

**One limit, stated plainly.** No CLI command drives a review loop yet, because
jjfx runs no reviewer of its own (`jjfx review` sends a prompt, it does not
produce findings). The caller wired here is the harness seam; the operator
surface it writes is the durable queue that `status --json` already exposes.
This is the same gap `09` disclosed, one layer further out.

Verified by `mise run check` (changelog validation + fmt + lint + build + test).

**Changelog:** `momo add` - a repeated review pass now replays instead of
reaching the reviewer, and the replay is visible in `status --json`'s queue.
