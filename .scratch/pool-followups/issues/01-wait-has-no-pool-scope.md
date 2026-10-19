# `jjfx wait` has no pool scope

Type: task
Status: resolved
Blocked by: none

## Problem

`WaitArgs` carries `--all` and `--until` and nothing else (`src/cli.rs:200-206`), so `jjfx wait` has no way to name a Worker Pool. With one pool - every Repository that declares nothing - that is invisible. With several it is the silent-wrong-pool failure the implementation deliberately removed from `dispatch`: the command acts on one lane (the implicit `default`) and neither refuses nor says which lane it meant.

Ticket 06 of the named-pools effort scoped `dispatch` to a pool and left `wait` alone, recording it as out of scope; the design map says `wait` narrows the same way.

## Solution

Resolve `wait`'s target pool the way `dispatch` does - the same `resolve_pool` seam, so the two cannot drift: one pool implies it, several require `--pool`, and an ambiguous or unknown name is refused naming the pools that exist. `--all` then means every Worker in the selected pool.

## Acceptance Criteria

- [x] `jjfx wait --all` with several pools refuses, naming the pools that exist.
- [x] `jjfx wait --pool <name> --all` waits on that lane's Workers only.
- [x] With one pool nothing changes, and `--pool` is not required.
- [x] An unknown pool name is refused with the same message `dispatch` gives.
- [x] `mise run check` is green.

## References

- `src/cli.rs` (`WaitArgs`, `wait_command`, `resolve_pool`), `src/cli_help.rs` (the `wait` page)
- `.scratch/named-pools/issues/06-dispatch-targets-a-pool.md`

## Resolution

`wait` now resolves its target pool through the same `resolve_pool` seam
`dispatch_command` uses: `WaitArgs` gains `--pool <name>`, `wait_command` reads
`config.pool_names_for`, resolves the pool and scopes the `Repository` handle
with `with_pool` before reading any state, and the flag is declared in
`WAIT_FLAGS` so the parser, the help page and the completion read one
definition. One pool implies it, so `--pool` stays optional and every
single-pool invocation is unchanged; several pools with no `--pool` refuse with
the `resolve_pool` message naming the count, the pools and `--pool <name>`, and
an unknown name with the message naming the pools that exist - both verbatim
`resolve_pool`, so the two commands cannot drift. `--all` now means every Worker
in the selected pool.

Verified by `mise run check` (green) and new cases in `tests/wait.rs`: several
pools refuse an unqualified `--all`; `--pool bugs --all` acts on that lane only
(the other lane's finished Worker does not satisfy the wait); a named Worker is
resolved in the selected lane; an unknown pool is refused with a message
byte-identical to `dispatch`'s; and one pool needs no `--pool`. `jjfx help wait`
and the zsh completion document `--pool`.
