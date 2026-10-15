# Pool-level cost/usage view and a dispatch spend cap

Status: ready-for-agent
Type: task
Blocked by: none

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
