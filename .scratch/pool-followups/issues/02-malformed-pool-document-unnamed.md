# A malformed pool document prints an unnamed diagnostic

Type: task
Status: resolved
Blocked by: none

## Problem

Ticket 17 removed the unnamed `pool state is absent` fragment that `pool list`, prose `status` and the TUI printed once per pool with no state directory. Its worker flagged what it deliberately left: a **malformed** pool document still prints an unnamed message (`cannot decode Worker Pool: ...`) to stderr.

Printing something is right - this is a real error, not the normal absent case, and ticket 17 was careful to keep absent and unreadable distinct. Not naming the pool is not: with several lanes a user cannot tell which one is corrupt, which is the same defect ticket 17 fixed for the absent case.

## Solution

Name the pool in the message, at the source that builds the diagnostic (`Repository::read_worker_pool_snapshot` in `src/engine/pool.rs`), so every surface that prints it - `pool list`, prose `status`, the TUI - names it too. Keep the absent-versus-malformed distinction intact.

## Acceptance Criteria

- [x] A malformed pool document's diagnostic names the pool it belongs to.
- [x] It still reaches stderr on `pool list` and prose `status`.
- [x] An absent pool document still prints nothing.
- [x] `mise run check` is green.

## Resolution

Named the pool where the diagnostic is built, in
`Repository::read_worker_pool_snapshot` (`src/engine/pool.rs`). The
`MalformedPool` message is now
`Worker Pool <name>: <error>`, so the three surfaces that print
`SnapshotDiagnostic::message()` - `pool list`, prose `status` (the same
`pool_list_command`) and the TUI (`src/render/pool.rs`) - all name the lane
without any printer being patched. The absent arm is untouched: `Loaded::Missing`
still returns no diagnostic, so a declared-but-unmaterialised lane stays silent
while an unreadable document still reports.

Verified by a manual drive (a repo declaring `alpha` with a malformed
`pool.json` and `beta` unmaterialised) which prints
`Worker Pool alpha: cannot decode Worker Pool: key must be a string at line 1
column 3` on both `pool list` and `status` and nothing for `beta`; and by three
new tests: an engine test in `tests/worker_pool.rs`
(`malformed_pool_diagnostic_names_the_pool`), a TUI render test in
`src/render/pool.rs`
(`the_pool_view_names_a_malformed_lane_in_its_diagnostic`), and an end-to-end
test in `tests/pool_status_json.rs`
(`a_malformed_pool_document_is_reported_naming_its_lane`). The existing
`missing_pool_is_not_invented_as_empty` and
`listing_declared_but_unmaterialised_pools_is_silent_on_stderr` tests continue
to pin the absent case silent. `mise run check` is green.

## References

- `src/engine/pool.rs` (`read_worker_pool_snapshot`, `SnapshotDiagnosticKind`)
- `.scratch/named-pools/issues/17-pool-list-stray-diagnostic.md`
