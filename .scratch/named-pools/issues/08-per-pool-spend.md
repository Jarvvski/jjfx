# Per-pool spend under the global ceiling

Type: task
Status: resolved
Blocked by: 06

## Problem

Spend is reported per Worker and pooled by the Repository. With several pools, a lane needs its own cap - and the interaction between that cap and the Repository-wide ceiling has to be decided, because a cap that silently drops work is exactly the failure class the dispatch-ergonomics batch exists to remove.

*How do a pool's spend cap and the global ceiling interact?* settled it:

- **A pool cap is the existing mechanism, scoped.** The global ceiling is `[spend]` in the user config - `cap_usd`, `window`, `on_exceed` - totalled from the durable per-Run cost records. A pool's cap takes the same shape in its declaration, defaulting `window` and `on_exceed` to the global values.
- **Both limits bind; the most restrictive wins, and the refusal names which.** When the ceiling is what ran out, the attribution names the **Repository**, because that is the dial to turn.
- **A cap-held Ticket lands in `waiting`, visibly.** An explicit single Dispatch refuses loudly instead, since the owner asked for that specific Ticket.
- **`--budget` is a cap inside the caps** and composes by the same rule.
- `on_exceed = warn` stays per-level.

## Acceptance Criteria

- [ ] A pool's declaration can carry a spend cap, defaulting window and mode to the global values.
- [ ] Dispatch proceeds only when every applicable limit has room, and the refusal names the binding one.
- [ ] A cap-held Ticket appears in `waiting`; an explicit single Dispatch refuses loudly.
- [ ] `--budget` composes as a cap inside the caps.
- [ ] `mise run check` is green.

## References

- `src/config.rs` (`SpendConfig`, `SpendWindow`, `SpendCapMode`)
- `src/cli.rs` (`dispatch_command`, the `waiting` set), `src/engine/state.rs` (Run Records)
- `.scratch/multi-pool/issues/06-pool-spend-caps.md`
