# A declared `default` pool's declaration goes stale

Type: task
Status: resolved
Blocked by: none

## Problem

Ticket 15 made `pool resize <N> --pool <name>` resize a materialised named lane **and** update its declaration, so the two agree. The same treatment was not applied to `default`: `jjfx pool <N>` (and `pool resize <N> --pool default`) still moves only the **state**. So a Repository that declares `[pools.default] size = 3` and then runs `jjfx pool 1` ends up with a declaration saying 3 and a lane holding 1.

Ticket 15's report flagged this as a symmetric hole it deliberately left, because its ticket named named pools. That was the right call for that ticket; this is the ticket it pointed at.

This is the **fourth instance** of the shape the effort's index already names - a declaration that does not reach the thing it describes. It is the mildest of the four, because `status` reads the materialised size first and so reports the truth; the lie is confined to the declaration. But it is the same lie, and it stops being mild the moment the state is lost and the declaration is what materialises the lane.

## Solution

Apply ticket 15's treatment to `default`: when a `default` lane is materialised and a declaration exists, resize the lane and update the declaration together, under the same busy-Worker safety (a refused resize changes neither).

When there is **no** `[pools.default]` declaration - the common case, since `default` is implicit - `jjfx pool <N>` must keep writing state only and must **not** create a declaration. Ticket 15's `apply_pool_resize` is the shared state half; the declaration half needs to become conditional.

## Acceptance Criteria

- [x] `jjfx pool <N>` on a Repository with a declared `[pools.default] size` updates both the lane and the declaration.
- [x] With no `[pools.default]` declaration, `jjfx pool <N>` behaves exactly as today and creates no declaration.
- [x] A refused resize (busy Worker) changes neither.
- [x] `status` reports a single size, and it matches the lane's Workers.
- [x] `mise run check` is green.

## References

- `src/cli.rs` (`apply_pool_resize`, `resize_declared_pool`, `set_pool_size`)
- `.scratch/named-pools/issues/15-resize-a-materialised-pool.md`
