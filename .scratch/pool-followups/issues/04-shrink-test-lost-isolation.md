# The pool-shrink test lost its isolation

Type: task
Status: ready-for-agent
Blocked by: none

## Problem

`a_failed_pool_shrink_leaves_no_cleanup_marker_on_a_member` (`tests/state_repositories.rs:293`) used to make `.jj` read-only so that only the manifest write failed while `.jj/pool` - where the cleanup marker lives - stayed writable. Ticket 02 moved the manifest and the markers into one directory (`.jj/pools/<name>/`), so that isolation is gone; the test now makes the pool directory read-only instead.

The assertion still holds - a failed shrink leaves no marker - but it no longer isolates the manifest write from the marker writes, so it can pass for the wrong reason. Ticket 02's worker flagged this and suggested revisiting it when the repository-wide/per-pool lock split landed; ticket 03 landed without it.

## Solution

Restore the isolation the test was built on, now that the two live in one directory: fail the manifest write specifically (a read-only file, or a deliberately malformed manifest) while leaving the marker path writable, so the test proves the marker is not written rather than that nothing is writable.

## Acceptance Criteria

- [ ] The test fails the manifest write specifically and leaves the marker path writable.
- [ ] It still asserts that a failed shrink leaves no cleanup marker.
- [ ] It fails if the marker write is attempted - verify by temporarily removing the guard.
- [ ] `mise run check` is green.

## References

- `tests/state_repositories.rs:293`, `src/engine/state.rs`
- `.scratch/named-pools/issues/02-persist-per-pool.md` and `03-pool-locks.md`
