# Order the cleanup marker after the pool manifest

Status: resolved
Type: task
Blocked by: none

## Problem

`shrink_pool` and `remove_pool_worker` wrote a `.cleanup` marker **before** the
Pool manifest. If the manifest write failed (disk full / EIO), the Worker was
still a Pool member but now carried a marker. `persist_reserved_pid` requires
`!cleanup_marker_path(...).exists()`, so every later dispatch to that Worker
reserved it, then failed PID persistence with `Conflict`, killed the child, and
released the reservation - permanently unusable until something cleared the
marker.

## Fix

Write the manifest **first**, then the marker. On a failed manifest write the
Worker keeps no marker; on a failed marker write the Worker is already detached,
so it is never dispatched and the leftover files are a harmless leak rather than
a stuck member.

## Commits

1. `Order the cleanup marker after the pool manifest` - both call sites, a
   regression test that fails without the fix, and a CHANGELOG `(fix)`.

## Testing Decisions

The regression test (`tests/state_repositories.rs`) makes `.jj` read-only so the
manifest's atomic write cannot create its temporary file, while `.jj/pool`
stays writable so the marker write would succeed. Verified to fail on the old
ordering (`worker-02.cleanup` existed) and pass on the new one.

## Acceptance Criteria

- [x] After a failed manifest write, a Worker is either a member with no marker
      or removed with a marker - never both.
- [x] The regression test fails on the pre-fix ordering and passes after.
- [x] `mise run check` is green.

## Out of Scope

- The malformed-marker discovery abort (C5, ticket 20).
