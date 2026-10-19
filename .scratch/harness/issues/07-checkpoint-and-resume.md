# An interruption must not orphan in-flight work

Status: ready-for-agent
Type: task
Blocked by: 06

## Problem

33 sessions were interrupted in five days, clustered in the review loop. The
drivers were killed after roughly five hours each, and **their in-flight
children died with them** - work that had been paid for and was never collected.

A workflow that is deterministic to run has to survive its own interruption:
what was in flight is recorded, and it can be resumed rather than lost or
silently re-done.

## Acceptance Criteria

- [ ] Interrupting a run leaves a durable record of what was in flight, not just
      a terminal status.
- [ ] In-flight work can be resumed without redoing completed steps.
- [ ] Resumption is explicit - an operator asks for it - rather than automatic
      retry of something that may have side effects.
- [ ] A test kills a run mid-flight and asserts the record survives and resumes.

## References

- `.scratch/harness/issues/06-durable-queue.md`
- `docs/run-records.md`, `src/engine/state.rs`
