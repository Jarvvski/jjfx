# Capture Run Records and spend

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

Spend is always zero, and no Run Record is written for opencode runs.

After five opencode runs that all finalized with `exit_code: 0`:

```
$ jjfx status
Spend: $0.0000 today, $0.0000 last 7 days, $0.0000 total across 0 Run(s); tokens in 0 out 0
```

Every Worker's `cost_usd` is `null`, and `.jj/pool/runs.jsonl` - which `docs/run-records.md` says every finalized Run appends a durable record to - does not exist at all. The data is available: the opencode event log contains per-step costs (`step_finish cost 0.0039036`).

So either the opencode finalization path never appends a Run Record, or Runs finalized by the lazy `reconcile_runs` path rather than by the spawning supervisor skip it. `RunRecords` has a writer (`src/engine/state.rs:2281`) and `RUN_RECORDS_PATH` is defined at `src/engine/state.rs:33`, so the plumbing exists.

This matters beyond a cosmetic zero: spend is a cap (`[spend] cap_usd`) and the Run Record is the durable history the pool is reconciled from - which is also what the writeback ticket wants to lean on.

## Solution

Find where an opencode Run's terminal state is observed, and make that the point a Run Record is appended - whether the Run ended under the supervisor or was reconciled later. Pull cost and token counts from the runtime's own event stream. The same fix should make `cost_usd` non-null in `status --json` and the `Spend:` line real.

Take care that a Run reconciled later is recorded exactly once.

## Acceptance Criteria

- [ ] A completed opencode Run appends one Run Record to `.jj/pool/runs.jsonl`.
- [ ] `status` and `status --json` report a non-zero cost after a run that reported one.
- [ ] A Run finalized by reconciliation rather than by the supervisor still produces a record, and no Run produces two.
- [ ] Tests cover record-once semantics.
- [ ] `mise run check` is green.

## References

- `src/engine/state.rs:33` (`RUN_RECORDS_PATH`), `:2281` (the writer), `records()`
- `src/engine/opencode_api.rs` (the opencode session and event stream)
- `src/engine/cost.rs`
- `docs/run-records.md`

## Comments

- 2026-10-16: Filed from the `todo-md` test-drive, where the report noted the costs were visible in the raw logs but absent from jjfx.
