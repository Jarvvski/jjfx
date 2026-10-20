# Drive the review loop through the decision log, so a repeat replays

Status: ready-for-agent
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

- [ ] A review pass is driven through the decision log, so a step already
      recorded replays rather than invoking the reviewer.
- [ ] The recurrence the parent ticket was filed for is measurably reduced: a
      repeated (ticket, dimension, pass) does not reach the model.
- [ ] An operator can see that a pass was replayed rather than run.

## References

- `.scratch/harness/issues/09-idempotent-steps.md`
- `src/engine/reviewer.rs` (`ReplayingReviewer`, `DecisionLog`)
- `docs/work-queue.md`
