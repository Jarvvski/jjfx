# Serialise every Workspace operation on the Worker lock

Status: resolved
Type: task
Blocked by: none

## Problem

`lock_worker_operation` serialised Workspace preparation and Reset restoration,
but only some callers took it:

- **Took it:** `direct_dispatch` -> `prepare_for_dispatch`; `reset`.
- **Skipped it:** `follow_up` (`send` / `review`) and `rebase`.

`reset` marks the Worker idle *before* a detached restoration thread runs
`jj restore` + `jj new main`. A follow-up dispatched in that window edited the
working copy mid-restore, corrupting it or losing work. `rebase` rewrote the
working copy with no serialisation at all.

## Fix

`follow_up` and `rebase` now take `lock_worker_operation`:

- `follow_up` acquires the lock **first**, so it blocks until any in-flight
  restoration finishes, and releases it at the Run's process-start handoff (via
  `run_reserved_foreground_with_handoff`, or right after
  `run_reserved_background`). It must not be held for the whole Run, or Reset
  could not interrupt a running Run.
- `rebase` holds it across the rebase/push/undo.

The lock ordering stays workspace-lock -> pool-lock, matching `reset` and
`prepare_for_dispatch`, so there is no inversion.

## Commits

1. `Serialise Follow-ups and rebases on the Worker operation lock` - both
   callers, a cross-process lock test, and a CHANGELOG `(fix)`.

## Testing Decisions

`send_waits_for_the_workspace_operation_lock` holds the same sidecar flock the
engine uses and asserts the `send` helper is still blocked after 400ms, then
releases and asserts it completes. It fails on the pre-fix code (which never
took the lock and returned immediately).

## Acceptance Criteria

- [x] No Workspace-mutating path runs without `lock_worker_operation`.
- [x] A Follow-up cannot interleave with a Reset restoration.
- [x] `mise run check` is green.

## Out of Scope

- `mount` does not mutate the working copy, so it is left unlocked.
- Replacing the file-lock implementation (ticket 25).
