# Order the cleanup marker after the pool manifest

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

`shrink_pool` (`state.rs:994-1001`) and `remove_pool_worker` (`:1045-1050`)
write a `.cleanup` marker **before** `write_atomic(POOL_PATH)`. If the manifest
write fails (disk full / EIO), the Worker is still a Pool member but now carries
a `.cleanup` marker. `persist_reserved_pid` requires
`!cleanup_marker_path(...).exists()` (`:920-921`), so every subsequent dispatch
to that Worker reserves it, then fails PID persistence with `Conflict`, kills
the child, and releases the reservation - the Worker is unusable until a
resize/remove happens to clear the stale marker.

## Fix

Make the durable writes ordered so a partial write cannot leave a marker for a
Worker that is still a Pool member:

- Write the manifest first, then the marker (or make the marker an explicit
  field of Worker state so it is atomic with the state).

## Commits

1. Reorder / re-represent the marker so marker-exists implies not-a-member.
2. Test: a failed manifest write leaves no marker for a member Worker.
3. CHANGELOG entry `(fix)`.

## Acceptance Criteria

- [ ] After any single write failure, a Worker is either a member with no marker
      or removed with a marker - never both.
- [ ] Dispatch to a Worker never fails permanently from a stale marker.
- [ ] `mise run check` is green.

## Out of Scope

- The malformed-marker discovery abort (C5, ticket 20).
