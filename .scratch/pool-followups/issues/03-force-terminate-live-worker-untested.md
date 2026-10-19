# `pool destroy --force` has no end-to-end test for a live busy Worker

Type: task
Status: resolved
Blocked by: none

## Problem

Ticket 05 made `pool destroy --force` the only path that discards work: it skips both refusals, says exactly what it will lose and what it will kill, and terminates the busy Worker's recorded process group. Its worker flagged that the kill path has **no end-to-end test** - killing an arbitrary recorded PID in a test is unsafe - so the mechanism is covered only indirectly, by the pre-existing `destroy_detached` test in `tests/worker_pool.rs`.

The refusal side and the `pid: null` forced path are tested. The live-kill path is the one an operator reaches for when a Worker is stuck, and it is the one nothing exercises.

## Solution

Make it testable rather than dangerous: start a real, harmless long-running process under a recorded process group the way the existing destroy tests do (a helper script, not an arbitrary PID), then drive `pool destroy --force` and assert the group is gone and the removal reports what it discarded. The repo already has the process-group helper pattern in `tests/support/` and `tests/worker_pool.rs`.

## Acceptance Criteria

- [x] A test drives `pool destroy --force` against a live busy Worker and asserts its process group is terminated.
- [x] The same test asserts the pool's state and the declaration are removed.
- [x] No test kills a process it did not start.
- [x] `mise run check` is green.

## Resolution

`tests/pool_removal.rs::destroy_force_terminates_a_live_worker_process_group` drives the real binary (`jjfx pool destroy bugs --force`) against a busy Worker whose persisted `pid` names a process group the test started. It reuses the existing helper pattern: an ignored `destroy_live_process_helper` test spawns `sh` with `process_group(0)`, records the leader's PID, and blocks on it, so the fixture owns the group it asserts on.

The test asserts the group is gone by polling `test_kill_process_group` (a signal-0 liveness probe, never a kill) to a deadline, and that the helper exited cleanly on `TERM` - the wait is bounded so a removal that never signals fails the test instead of hanging the suite. A negative check with the PID left unrecorded failed at the deadline as intended. It then asserts the Pool's state directory (`pool_state("bugs")`) and its `[pools.bugs]` declaration are both removed, and that the forced path printed `--force terminates`.

`mise run check` is green. No changelog entry: a test-only change with no user-visible effect.

## References

- `tests/pool_removal.rs`, `tests/worker_pool.rs` (`destroy_terminates_a_live_recorded_process_group`), `tests/support/`
- `.scratch/named-pools/issues/05-pool-removal-drains.md`
