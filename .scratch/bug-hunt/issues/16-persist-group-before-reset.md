# Persist the Dispatch Group before resetting the Worker

Status: resolved
Type: task
Blocked by: none

## Problem

The Done and Retry paths ordered their durable writes oppositely:

- Done (`orchestration.rs`): apply -> `save_group` -> observe -> `reset_worker`.
- Retry: `reset_worker` **first**, then apply/save.

If `save_group` failed after `reset_worker` succeeded, the Worker was idle with
its ticket cleared while the persisted group still said `dispatched`. On restart,
reconciliation saw `observation.ticket != ticket` and errored
`WorkerTicketMismatch`, so the parent could never be resumed.

## Fix

The retry path now commits the group before mutating the Worker, matching the
Done path: apply `Failed` -> apply `Retried` -> `save_group` -> `reset_worker` ->
observe.

## Commits

1. `Persist the Dispatch Group before resetting a retried Worker` - the reorder,
   the updated expectations, a new conflict test, and a CHANGELOG `(fix)`.

## Testing Decisions

The existing `FakeExecution` in `orchestration.rs` already records an ordered
`calls` log of `save:`/`reset:`, so the ordering is asserted directly rather than
inferred:

- `retry_save_conflict_leaves_the_worker_bound_and_the_group_resumable` - with
  `fail_next_save`, the calls are `["workers", "save:r0:ENG-101:pending"]`: no
  `reset:` follows the failed save, and the group still says `dispatched`.
  It fails on the pre-fix code, which reset first.
- `first_failed_run_persists_one_retry_before_resetting_the_worker` - the happy
  path now asserts `save` before `reset` (previously the reverse).
- `reset_failure_after_a_durable_retry_stops_the_run` - a Reset failure now
  leaves the group durably `pending` with `retries == 1`, so a restart resumes
  rather than reconciling against a cleared Worker.

## Acceptance Criteria

- [x] A `save_group` conflict on the retry path leaves a resumable state.
- [x] No terminal transition mutates the Worker before persisting the group.
- [x] `mise run check` is green.
