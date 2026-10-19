# The Linear branch of the empty-ready-set `root` is untested

Type: task
Status: ready-for-agent
Blocked by: none

## Problem

Ticket 19 added a `Search` entry to `dispatch --all --json` so an empty ready set carries its reason as structure. The entry names the label searched and the `root` searched, and `root` is `null` for a Linear source - there is no filesystem root to name. That branch is covered by a unit test only; ticket 19's worker could not exercise a real Linear source end to end.

A `null` field is the kind a consumer reads without checking, so it is worth an integration test that proves the shape rather than a unit test that proves the encoder.

## Solution

Cover the Linear path end to end. If a live Linear source cannot be driven in the suite - it needs credentials - then drive the seam that selects the source and assert the emitted `root` is `null` for it, and say in the test why the query itself is not exercised.

## Acceptance Criteria

- [ ] An integration test asserts the `root` field is `null` for a Linear source.
- [ ] The test explains why the query is not exercised, if it is not.
- [ ] `mise run check` is green.

## References

- `src/cli.rs` (`search_entry`, `dispatch_json`), `src/ticket_source.rs`
- `.scratch/named-pools/issues/19-json-never-explains-an-empty-ready-set.md`
