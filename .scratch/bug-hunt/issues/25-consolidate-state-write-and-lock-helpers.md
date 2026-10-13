# Consolidate the atomic-write and lock helpers

Status: resolved
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

- [x] One atomic-write implementation and one lock implementation in the tree.
- [x] No behaviour change for existing callers.
- [x] `mise run check` is green.

## Outcome (2026-10-13)

**New module `src/engine/atomic.rs`** holding both primitives:

- `write_atomic_bytes(path, bytes)` - parent `create_dir_all`, `NamedTempFile`
  in the target directory, mode `0644`, write / flush / `sync_all`, then rename
  over the target. `state::write_atomic` now serializes its JSON and delegates;
  `workspace::write_cache` builds its line text and delegates.
- `lock_exclusive` / `try_lock_exclusive` on `rustix::flock`, plus a
  `LockOutcome` for the non-blocking case. `state::with_locks`,
  `workspace::with_cache_lock`, `workspace::lock_worker_operation`, and
  `orchestration::acquire_lock` all use it; the old `File::lock()` and the
  inline `open`+`flock` copies are gone.

Both return a step-tagged error (`AtomicWriteError` / `LockError`) rather than a
single opaque failure, so every caller keeps its own contextual wording while
sharing identical failure semantics - which is why this needed no shared error
type.

**One real behaviour change, stated deliberately:** ws-cache writes now use a
uniquely named temporary file (random, auto-removed on failure) with mode `0644`
and an fsync, replacing the deterministic `ws-cache.<pid>.<thread>.tmp` written
with `fs::write`. This is a **conformance fix**, not a regression: the
compatibility contract
(`docs/wsg-compatibility-contract.md:130-138`) already requires a *uniquely
named* temporary file at mode `0644`, cleaned up on failure. The old code
violated both halves. The test that used to force a failure by pre-creating the
predictable temp path was updated accordingly: since a fixed-path collision can
no longer fail the write, the injection now removes write permission from `.jj`
(restored by the teardown hook) so only the cache temp-file step fails. The
asserted behaviour - full rollback, error text `"write ws-cache temporary
file"` - is unchanged.

**Deliberately left divergent:** `write_destroy_marker` / `write_cleanup_marker`
stay as in-place create+truncate+`fsync`. Marker *existence* is the signal and
there is no previous document to preserve, so temp+rename is the wrong
primitive rather than a duplicate of one. `hooks.rs` and `events.rs` temp+rename
writes are untouched: outside the engine state contract and outside the ticket's
scope. No parent-directory fsync was added - the contract excludes it, and this
ticket is behaviour-preserving.

**Tests (2 added):** `atomic::a_failed_replacement_leaves_the_previous_target_intact`
(the previous bytes survive a failed replacement and no temp file is left
behind) and `atomic::a_missing_parent_is_reported_without_touching_the_filesystem`.

**Residual risk:** for rare OS-level flock/open failures the error text now
renders through `io::Error` and may carry an `(os error N)` suffix that
rustix's `Errno` display omitted. No test pins that text.

## Out of Scope

- Changing the persisted document format.
