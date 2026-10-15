# Pool-level cost/usage view and a dispatch spend cap

Status: resolved
Type: task
Blocked by: 16

## Problem

jjfx records per-Run cost, usage, duration, and turns (`CONTEXT.md`, Run result;
`CHANGELOG.md`), and shows a per-Worker usage line (`src/render/pool.rs:340`
`worker_usage_line`). But there is no **pool-level** cost view and no **spend
gate** on Dispatch, so a pool of parallel agents can run unbounded and the owner
cannot see aggregate spend without reading each Worker.

`max_budget_usd` exists on the runtime (`src/engine/runtime.rs:259`) but is a
per-Run hint that Pi rejects; there is no aggregate cap.

## Solution

- Aggregate the per-Run costs jjfx already records into a **pool cost/usage view**
  (e.g. `$` today / this week, and per Worker).
- Add an optional **spend cap** in config that gates or warns on Dispatch when the
  aggregate would exceed it.

Optional: reconcile against the provider's usage/cost API for truth, but the
recorded per-Run costs are sufficient for a first cut.

## Commits

1. Aggregate recorded Run costs/usage across the Pool into a snapshot.
2. Render the pool cost/usage view in the Pool view (and a `jjfx status` column
   or summary line).
3. Add a config spend cap and gate Dispatch on it (block or warn, configurable),
   with a clear message naming the cap and the override.
4. Tests; docs (`docs/configuration.md`); changelog.

## Acceptance Criteria

- [ ] The Pool view shows aggregate cost/usage for the pool, not just per Worker.
- [ ] A configured cap is enforced at Dispatch with a clear, actionable message.
- [ ] A Run with an unrepresentable cost does not break aggregation (per the
      bug-hunt ticket 28 handling).
- [ ] `mise run check` is green.

## Out of Scope

- Provider API reconciliation (a possible follow-up).
- Per-Run budget passthrough (already exists as `max_budget_usd`).

## References

- `src/render/pool.rs:340`, `src/engine/runtime.rs:259`, `CONTEXT.md` (Run result)
- Anthropic usage/cost API (optional reconciliation):
  https://platform.claude.com/docs/en/manage-claude/usage-cost-api

## Comments

- 2026-10-15: Filed from the strategy review.
- 2026-10-15: **Premise corrected - this ticket cannot be built as written.**
  It aggregates "the per-Run costs jjfx already records", but jjfx does not retain
  them: `apply_run_result` (`src/engine/pool.rs:1529-1544`) persists only
  status/exit_code/error, and the Run log is opened `.truncate(true)`
  (`src/engine/runtime.rs:403-411`), so each Run overwrites the previous one's
  cost. Only the latest Run per Worker survives. Re-marked `Blocked by: 16`,
  which adds the durable per-Run record this needs.
- 2026-10-15: Landed, on the ticket 16 record store. `engine/cost.rs` folds the
  durable records into `PoolCostSnapshot` - a pool total, today and the last
  seven days, and a per-Worker split, with Runs whose provider reported no usable
  cost counted apart from a zero spend - and `WorkerPoolSnapshot` carries it, so
  both frontends read it from the snapshot they already have. The Pool view shows
  a pool spend/usage line and a per-Worker spend on each row; `jjfx status` (and
  `pool list`) shows a `SPEND` column, a pool-level summary, and `cost_usd` per
  Worker in its JSON. A new optional `[spend]` section (`cap_usd`, `window`,
  `on_exceed`) gates Dispatch from the same aggregate: `block` refuses before any
  Worker is reserved with a message naming the cap, the current total and the
  override; `warn` proceeds and reports the overage (the TUI rides it on the
  Dispatch status, the CLI prints it). Both Dispatch paths are gated - the TUI
  adapter's Direct Dispatch and orchestration, and `jjfx dispatch` including its
  orchestration handoff. Pinned by 9 engine aggregation/cap unit tests, 4 adapter
  gate unit tests, 2 `jjfx status` unit tests, a render test and a Pool session
  test, plus 6 config parsing/validation tests. Documented in
  `docs/configuration.md`. Decisions: `week` is a rolling seven days rather than a
  calendar week (the total resets continuously instead of jumping at a boundary);
  the cap fires at or above its limit; and a cap that cannot be evaluated because
  the record store is unreadable refuses Dispatch rather than guessing. The
  provider usage/cost API reconciliation stays out of scope, as the ticket says.
