# The pool-shrink test lost its isolation

Type: task
Status: resolved
Blocked by: none

## Problem

`a_failed_pool_shrink_leaves_no_cleanup_marker_on_a_member` (`tests/state_repositories.rs:293`) used to make `.jj` read-only so that only the manifest write failed while `.jj/pool` - where the cleanup marker lives - stayed writable. Ticket 02 moved the manifest and the markers into one directory (`.jj/pools/<name>/`), so that isolation is gone; the test now makes the pool directory read-only instead.

The assertion still holds - a failed shrink leaves no marker - but it no longer isolates the manifest write from the marker writes, so it can pass for the wrong reason. Ticket 02's worker flagged this and suggested revisiting it when the repository-wide/per-pool lock split landed; ticket 03 landed without it.

## Solution

Restore the isolation the test was built on, now that the two live in one directory: fail the manifest write specifically (a read-only file, or a deliberately malformed manifest) while leaving the marker path writable, so the test proves the marker is not written rather than that nothing is writable.

## Acceptance Criteria

- [x] The test fails the manifest write specifically and leaves the marker path writable.
- [x] It still asserts that a failed shrink leaves no cleanup marker.
- [x] It fails if the marker write is attempted - verify by temporarily removing the guard.
- [x] `mise run check` is green.

## Resolution

The test now fails the manifest write specifically. The manifest's atomic replacement creates its temporary file in the pool directory, so the pool directory is still made read-only - that is what fails the manifest write. The cleanup markers live in that same directory, so to keep the marker path writable the test points `worker-02.cleanup` at a writable file outside the pool directory with a symlink. The production code decides whether a marker is present with `Path::exists()`, which follows the link, so the dangling link reads as "no marker" in the passing case; an attempted marker write would create the link target and fail the assertion.

Verified load-bearing by temporarily changing `shrink_pool` to ignore the manifest write's error (`let _ = write_atomic(...)`): the test then fails on the marker assertion, because the marker write follows the link and lands. The guard was restored, and `mise run check` is green.

Note: the ticket suggested a read-only file as one option. That does not work here - the atomic replacement is a `rename`, which ignores the target file's permissions when the directory is writable, so a read-only `pool.json` lets the shrink succeed rather than fail.

## References

- `tests/state_repositories.rs:293`, `src/engine/state.rs`
- `.scratch/named-pools/issues/02-persist-per-pool.md` and `03-pool-locks.md`
