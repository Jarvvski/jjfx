# `pool resize --pool <name>` does not resize a materialised pool

Type: task
Status: ready-for-agent
Blocked by: none

## Problem

Ticket 04 made `pool resize <N> --pool <name>` edit the **declaration**, on the reasoning that "the declaration is the source of truth for a declared field". Ticket 13 then made a declared pool materialise on first Dispatch, **applying the declared size** to provision its Workers.

Those two decisions do not compose. Once a lane has materialised, resizing its declaration changes nothing: the lane keeps the Workers it was provisioned with. So `[pools.bugs] size = 3` and a `bugs` lane holding two Workers can both be true at once, and nothing says which one `status` is reporting.

Ticket 13's report flagged this as pre-existing and out of its scope. It is not pre-existing - it is created by the pair, and it is the same failure this effort keeps producing in miniature: **a declaration that does not reach the thing it describes.** It is worse than an unused key, because the user's edit *looks* like it worked.

## Solution

Make a declaration edit reach a materialised lane, or make the divergence impossible.

- **Resize the lane when it exists** (recommended): `pool resize <N> --pool <name>` applies to the materialised pool the way `jjfx pool <N>` already does for `default`, growing and shrinking under the existing safety rules - never shrinking over a busy Worker. The declaration is updated too, so the two agree.
- **Or make the declaration read-only once materialised**, refusing the edit with the reason and pointing at the verb that works. Honest, but it means a size can never be changed after first use, which is probably not what anyone wants.

Whichever is chosen, `status` must report **one** size, and it must be the size that is true.

## Acceptance Criteria

- [ ] A size edit after materialisation either takes effect on the lane, or is refused with a reason.
- [ ] `status` reports a single size, and it matches the lane's Workers.
- [ ] Shrinking a materialised lane keeps the existing busy-Worker safety.
- [ ] `mise run check` is green.

## References

- `src/cli.rs` (`resize_pool_command`, `materialise_declared_pool`), `src/engine/pool.rs` (`resize_to`, `materialise`)
- `.scratch/named-pools/issues/04-pool-lifecycle.md` and `13-materialise-a-declared-pool.md`
