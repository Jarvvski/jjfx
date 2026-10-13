# Consolidate the atomic-write and lock helpers

Status: ready-for-agent
Type: task
Blocked by: none

## Improvement

Two modules implement the same durability primitives differently:

- `state.rs` uses `rustix::flock` + `NamedTempFile` + `write_atomic`
  (`:2077-2107`) and marker writers (`:1941-1963`).
- `workspace.rs` reimplements temp+rename for the ws-cache (`write_cache`,
  `:1389-1413`) and uses a different lock API (`File::lock`).

The divergence is where several bugs live (marker ordering, no directory fsync,
temp-file leaks on crash). The compatibility contract explicitly excludes
parent-directory fsync today (`docs/wsg-compatibility-contract.md:137`), so this
ticket is about **one** helper, not a durability policy change.

## Fix

Extract one atomic-write + one file-lock helper and route both modules through
it. Optionally add parent-directory fsync behind the helper and record the
policy change in the contract doc (it is currently out of scope by contract).

## Commits

1. Extract the shared helper(s); port `state.rs`.
2. Port `workspace.rs`.
3. Tests: identical failure semantics for both callers.
4. CHANGELOG entry `(add)` (internal; skip if no user-visible change).

## Acceptance Criteria

- [ ] One atomic-write implementation and one lock implementation in the tree.
- [ ] No behaviour change for existing callers.
- [ ] `mise run check` is green.

## Out of Scope

- Changing the persisted document format.
