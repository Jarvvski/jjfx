# `jjfx wait` has no pool scope

Type: task
Status: ready-for-agent
Blocked by: none

## Problem

`WaitArgs` carries `--all` and `--until` and nothing else (`src/cli.rs:200-206`), so `jjfx wait` has no way to name a Worker Pool. With one pool - every Repository that declares nothing - that is invisible. With several it is the silent-wrong-pool failure the implementation deliberately removed from `dispatch`: the command acts on one lane (the implicit `default`) and neither refuses nor says which lane it meant.

Ticket 06 of the named-pools effort scoped `dispatch` to a pool and left `wait` alone, recording it as out of scope; the design map says `wait` narrows the same way.

## Solution

Resolve `wait`'s target pool the way `dispatch` does - the same `resolve_pool` seam, so the two cannot drift: one pool implies it, several require `--pool`, and an ambiguous or unknown name is refused naming the pools that exist. `--all` then means every Worker in the selected pool.

## Acceptance Criteria

- [ ] `jjfx wait --all` with several pools refuses, naming the pools that exist.
- [ ] `jjfx wait --pool <name> --all` waits on that lane's Workers only.
- [ ] With one pool nothing changes, and `--pool` is not required.
- [ ] An unknown pool name is refused with the same message `dispatch` gives.
- [ ] `mise run check` is green.

## References

- `src/cli.rs` (`WaitArgs`, `wait_command`, `resolve_pool`), `src/cli_help.rs` (the `wait` page)
- `.scratch/named-pools/issues/06-dispatch-targets-a-pool.md`
