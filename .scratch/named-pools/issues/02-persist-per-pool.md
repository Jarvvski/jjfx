# Persist a directory per pool, converting an existing one

Type: task
Status: ready-for-agent
Blocked by: none

## Problem

The persisted state was built for exactly one pool: a pool document at `.jj/pool.json` beside a directory `.jj/pool/` holding the Workers, Run Records, the waiting set, the two jjfx-owned sidecar documents and six lock files. Nothing in that layout says which state is a pool's and which is the Repository's - a question that could not arise while there was only one pool.

**ADR 0013** settles it, and *Is the pool index or the pool directories the source of truth?* drops the index:

- **A directory per pool**: `.jj/pools/<name>/` holds the pool document, its Workers, Run Records, the waiting set, occupancy, sessions and its locks.
- **The principle is the split itself**: pool-scoped state lives under the pool; repository-scoped state stays at `.jj/`. So `ws-cache` and `undo.json` stay at the root - undo because it is `jj op restore`, and that restore is repository-wide.
- **No index.** The `[pools]` declaration is the source of truth and a pool directory is state, so `.jj/pools.json` is dropped and there is nothing for an index to disagree with.
- **The sidecars stay files** (`in-use.json` per Repository, `sessions.json` per pool): occupancy is written wholesale, so as one file it is one atomic replace, and as per-Worker fields it would be N read-modify-writes with no single point of consistency.

## Scope

- Move every path in `src/engine/state.rs` to the per-pool layout, including the pool document (which stops being a file *beside* a directory and becomes one *inside* it).
- **Automatic one-time conversion on first access**: detect the legacy layout, build the new one, **verify it, and only then remove the old**. It copies before it removes and refuses if it cannot verify the result.
- The conversion is safe only because the Go peer is dead - there is no second reader to keep the legacy layout alive. It must still never destroy a Worker Pool.
- An **orphan directory** (a pool directory with no matching declaration) is reported by name and never adopted; a **declared pool with no directory** is normal, and materialises on first use.

## Acceptance Criteria

- [ ] State lives at `.jj/pools/<name>/`, with `ws-cache` and `undo.json` still at `.jj/`.
- [ ] An existing Repository converts automatically on first access, and the conversion is verified before the old layout is removed.
- [ ] A conversion that cannot be verified leaves the old layout intact and fails loudly.
- [ ] No `.jj/pools.json` is written or read.
- [ ] An orphan pool directory is reported, not adopted.
- [ ] `mise run check` is green.

## References

- `src/engine/state.rs` (`POOL_PATH`, `POOL_DIRECTORY`, and the per-state path/lock constants)
- `docs/adr/0013-persist-pool-state-per-pool.md`
- `.scratch/multi-pool/issues/03-pool-state-layout-and-migration.md` and `12-index-or-directories.md`
