# A declared pool's state is never materialised

Type: task
Status: ready-for-agent
Blocked by: none

## Problem

Ticket 02 promised that "a declared pool with no directory is normal, and materialises on first use". The first half is implemented - the absence is tolerated, and an orphan is reported - but the second half is not, and nothing later in the effort took it up:

- Ticket 04 stops at the declaration and says so: "No supported path materialises a named pool's state directory yet, so nothing is orphaned in practice."
- Ticket 07's integration tests had to **provision named-pool state directly**, the way `tests/pool_removal.rs` does, because the product will not.

So the last mile is missing. A Repository can declare `[pools.bugs]`, and `dispatch --pool bugs` into it fails with `state is missing`. Every named lane is declared-but-unusable, which means the feature does not work end to end - only its plumbing does.

## Solution

Materialise a pool's state on first use, as ticket 02 said. The state layer already carries the notion - `StateStore` knows "whether the Pool has ever materialised its state directory" - so this is about reaching it from the path that needs it, rather than only from a write.

Decide and record **where "first use" is**: `Repository::with_pool` / `state_store_for_pool`, or the first operation that needs the pool's document. Make it idempotent, and safe under the repository-outermost lock order ticket 03 fixed. A pool that is declared and then removed must leave nothing behind - ticket 05 already removes the directory, so this must not resurrect it.

## Acceptance Criteria

- [ ] `dispatch --pool <name>` into a freshly declared pool works with no manual provisioning.
- [ ] Materialisation is idempotent, and safe under the ticket-03 lock order.
- [ ] The integration tests that provision named-pool state by hand can drop that setup.
- [ ] `mise run check` is green.

## References

- `src/engine/state.rs` (`StateStore`, the materialised-directory notion), `src/engine/pool.rs`
- `.scratch/named-pools/issues/02-persist-per-pool.md` (its "materialises on first use" clause)
- `.scratch/named-pools/issues/04-pool-lifecycle.md` and `07-route-by-label.md` (both record the gap)
