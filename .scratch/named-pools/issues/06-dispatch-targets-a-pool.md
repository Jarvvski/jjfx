# Dispatch targets a pool

Type: task
Status: resolved
Blocked by: 01, 02

## Problem

Every Dispatch assumes the one pool. With several, a Dispatch has to know which lane it is filling - and the answer must not be a guess.

*How does a Ticket reach a pool?* settled the primitive, and it is deliberately about **the number of pools, not about sources**:

- **One pool implies it; several require `--pool`.** With one pool - every existing Repository - nothing changes, which is the additive stance. With several, an explicit id must name its pool.
- Resolving by *which source knows the id* was rejected as the primitive: it cannot disambiguate two pools on the same tracker (the common case), and it costs a discovery query per pool on every Dispatch. It stays available later as a convenience.
- **`--text` and `--from-file` follow the same rule**: `default` when there is one pool, `--pool` required when there are several.
- An ambiguous or missing pool is a **loud refusal naming the pools that exist**, never a silent choice of one.

## Scope

- `dispatch` resolves its target pool: explicit `--pool`, else the single pool when there is one, else a loud refusal.
- Worker reservation, Dispatch Eligibility and the pool's size all apply to the *selected* pool.
- The existing single-pool invocations keep working untouched.

## Acceptance Criteria

- [x] With one pool, `jjfx dispatch <TICKET>` behaves exactly as today.
- [x] With several, an unqualified Dispatch is refused, naming the pools that exist.
- [x] `--pool <name>` selects the lane, and `--text` / `--from-file` follow the same rule.
- [x] Eligibility, occupancy and size are all evaluated against the selected pool.
- [x] `mise run check` is green.

## References

- `src/cli.rs` (`dispatch_command`), `src/engine/pool.rs` (reservation, eligibility)
- `.scratch/multi-pool/issues/04-how-tickets-reach-a-pool.md`
