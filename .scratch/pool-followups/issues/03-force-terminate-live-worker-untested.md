# `pool destroy --force` has no end-to-end test for a live busy Worker

Type: task
Status: ready-for-agent
Blocked by: none

## Problem

Ticket 05 made `pool destroy --force` the only path that discards work: it skips both refusals, says exactly what it will lose and what it will kill, and terminates the busy Worker's recorded process group. Its worker flagged that the kill path has **no end-to-end test** - killing an arbitrary recorded PID in a test is unsafe - so the mechanism is covered only indirectly, by the pre-existing `destroy_detached` test in `tests/worker_pool.rs`.

The refusal side and the `pid: null` forced path are tested. The live-kill path is the one an operator reaches for when a Worker is stuck, and it is the one nothing exercises.

## Solution

Make it testable rather than dangerous: start a real, harmless long-running process under a recorded process group the way the existing destroy tests do (a helper script, not an arbitrary PID), then drive `pool destroy --force` and assert the group is gone and the removal reports what it discarded. The repo already has the process-group helper pattern in `tests/support/` and `tests/worker_pool.rs`.

## Acceptance Criteria

- [ ] A test drives `pool destroy --force` against a live busy Worker and asserts its process group is terminated.
- [ ] The same test asserts the pool's state and the declaration are removed.
- [ ] No test kills a process it did not start.
- [ ] `mise run check` is green.

## References

- `tests/pool_removal.rs`, `tests/worker_pool.rs` (`destroy_terminates_a_live_recorded_process_group`), `tests/support/`
- `.scratch/named-pools/issues/05-pool-removal-drains.md`
