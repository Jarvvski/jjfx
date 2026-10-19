# A malformed pool document prints an unnamed diagnostic

Type: task
Status: ready-for-agent
Blocked by: none

## Problem

Ticket 17 removed the unnamed `pool state is absent` fragment that `pool list`, prose `status` and the TUI printed once per pool with no state directory. Its worker flagged what it deliberately left: a **malformed** pool document still prints an unnamed message (`cannot decode Worker Pool: ...`) to stderr.

Printing something is right - this is a real error, not the normal absent case, and ticket 17 was careful to keep absent and unreadable distinct. Not naming the pool is not: with several lanes a user cannot tell which one is corrupt, which is the same defect ticket 17 fixed for the absent case.

## Solution

Name the pool in the message, at the source that builds the diagnostic (`Repository::read_worker_pool_snapshot` in `src/engine/pool.rs`), so every surface that prints it - `pool list`, prose `status`, the TUI - names it too. Keep the absent-versus-malformed distinction intact.

## Acceptance Criteria

- [ ] A malformed pool document's diagnostic names the pool it belongs to.
- [ ] It still reaches stderr on `pool list` and prose `status`.
- [ ] An absent pool document still prints nothing.
- [ ] `mise run check` is green.

## References

- `src/engine/pool.rs` (`read_worker_pool_snapshot`, `SnapshotDiagnosticKind`)
- `.scratch/named-pools/issues/17-pool-list-stray-diagnostic.md`
