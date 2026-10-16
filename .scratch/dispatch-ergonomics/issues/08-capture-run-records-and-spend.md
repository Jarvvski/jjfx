# Capture Run Records and spend

Status: resolved
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

- [x] A completed opencode Run appends one Run Record to `.jj/pool/runs.jsonl`.
- [x] `status` and `status --json` report a non-zero cost after a run that reported one.
- [x] A Run finalized by reconciliation rather than by the supervisor still produces a record, and no Run produces two.
- [x] Tests cover record-once semantics.
- [x] `mise run check` is green.

## References

- `src/engine/state.rs:33` (`RUN_RECORDS_PATH`), `:2281` (the writer), `records()`
- `src/engine/opencode_api.rs` (the opencode session and event stream)
- `src/engine/cost.rs`
- `docs/run-records.md`

## Comments

- 2026-10-16: Filed from the `todo-md` test-drive, where the report noted the costs were visible in the raw logs but absent from jjfx.
- 2026-10-16: Root cause was two-fold. (1) A background Dispatch spawns a waiter thread in the CLI process, but the CLI exits before the Run ends, so the Run is always finalized by the lazy `reconcile_runs` path - and `reconcile_worker` applied the terminal result to Worker state without appending a Run Record. It now appends on the `Applied` commit only, so the supervisor and reconciliation paths record each Run exactly once. (2) The opencode log parser never read the per-step `step_finish` cost/tokens, so even a recorded Run carried no cost; the Run's totals are now summed from its own event log in both the supervisor and reconciliation paths.
