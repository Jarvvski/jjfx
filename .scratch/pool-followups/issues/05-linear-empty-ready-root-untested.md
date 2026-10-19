# The Linear branch of the empty-ready-set `root` is untested

Type: task
Status: resolved
Blocked by: none

## Problem

Ticket 19 added a `Search` entry to `dispatch --all --json` so an empty ready set carries its reason as structure. The entry names the label searched and the `root` searched, and `root` is `null` for a Linear source - there is no filesystem root to name. That branch is covered by a unit test only; ticket 19's worker could not exercise a real Linear source end to end.

A `null` field is the kind a consumer reads without checking, so it is worth an integration test that proves the shape rather than a unit test that proves the encoder.

## Solution

Cover the Linear path end to end. If a live Linear source cannot be driven in the suite - it needs credentials - then drive the seam that selects the source and assert the emitted `root` is `null` for it, and say in the test why the query itself is not exercised.

## Acceptance Criteria

- [x] An integration test asserts the `root` field is `null` for a Linear source.
- [x] The test explains why the query is not exercised, if it is not.
- [x] `mise run check` is green.

## Resolution

`tests/dispatch_outcomes.rs` gains
`a_linear_source_reports_a_null_search_root_in_json`, which drives the real
binary end to end. It selects the source the way a user does - a user config
carrying `[tickets] source = "linear"` - and asserts the emitted `--json` batch
names the search with `root: null` and `label: "ready-for-agent"`.

The query itself is not exercised, and the test says so in its doc comment: a
real Linear query needs Linear credentials and a reachable Agent Runtime, so
the fixture stubs the Runtime (a fake `claude` on `PATH`) with the empty ready
set `{"tickets":[]}`. That reaches the empty-ready-set branch that emits the
search entry, which is the shape under test; it is not evidence that jjfx can
discover real Linear Tickets. The fixture helper `with_empty_ready_query`
carries the same reasoning.

The local-source path is unchanged: `an_empty_ready_set_carries_the_search_in_json`
still asserts a filesystem root ending in `.scratch`, and no production code
changed. `mise run check` is green.

## References

- `src/cli.rs` (`search_entry`, `dispatch_json`), `src/ticket_source.rs`
- `.scratch/named-pools/issues/19-json-never-explains-an-empty-ready-set.md`
