# The per-pool `ready_status` is declared but unused

Type: task
Status: ready-for-agent
Blocked by: 07

## Problem

Ticket 01 added `ready_status` to `[pools.<name>]` - "the ready label is per pool, defaulting to the global value" - and ticket 07 used the pool's `label` for routing but left `ready_status` alone. Ticket 07's report says so plainly: "`[pools.<name>] ready_status` is still declared-but-unused... left for whichever ticket owns it."

A configuration key that parses, validates, and does nothing is exactly the silent no-op this project has spent a whole batch removing. A user who sets it will believe it took effect.

## Solution

Either make it act or remove it. Both are honest; leaving it is not.

The likely answer is to make it act: a pool's ready status should filter its own discovery the way its `label` already does, so a lane can define readiness differently - a review pool drawing `ready-for-review` while the default draws `ready-for-agent`. That is what the design meant by a ready label being per pool.

If it turns out the design does not need it, remove the key and say why. A documented absence beats a silent one.

## Acceptance Criteria

- [ ] `[pools.<name>] ready_status` either filters that pool's discovery, or the key is gone.
- [ ] Whichever is chosen, `docs/configuration.md` matches.
- [ ] No configuration key parses and does nothing.
- [ ] `mise run check` is green.

## References

- `src/config.rs` (`PoolConfig`), `src/engine/local_tickets.rs`
- `.scratch/named-pools/issues/01-declare-pools.md` and `07-route-by-label.md`
