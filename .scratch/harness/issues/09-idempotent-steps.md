# Steps keyed by identity, so a re-run does not duplicate work

Status: ready-for-agent
Type: task
Blocked by: 06

## Problem

The same work is done repeatedly and unknowingly. The seven review dimensions
recur **8-11x** as duplicate titles across rounds, and 54 sessions are
`(fork #N)` re-forks of long conversations - one title 33 times.

Nothing keys a step to an identity, so nothing can tell "this has already been
done" from "this needs doing". The research's pattern is to make
side-effecting steps idempotent and replay recorded decisions, so a retry is
safe and a repeat is detectable.

## Acceptance Criteria

- [ ] A step has an identity derived from what it is (ticket, dimension, pass),
      recorded durably.
- [ ] Re-running a completed step is detectable and reported, not silently
      repeated.
- [ ] Replay of a recorded decision does not re-invoke the model.
- [ ] A test drives the same step twice and asserts the second is recognised.

## References

- `.scratch/harness/issues/06-durable-queue.md`
- `src/engine/state.rs` (`RunRecord`)
