# Persist a durable per-Run cost record

Status: resolved
Type: task
Blocked by: none
Blocks: 07

## Problem

jjfx **parses** cost and usage for every Run and then **throws them away**.

`RunUsage` (`src/engine/run_log.rs:1519-1583`) and `RunCost` (`:1799-1813`) are
normalised per Run from the runtime's log events, and `RunResult` carries
`usage`, `cost`, `duration` and `turns` (`:1827-1910`). The Pool view renders a
per-Worker usage line from the **latest activity** (`src/render/pool.rs:340`).
But `apply_run_result` (`src/engine/pool.rs:1529-1544`) persists only
`status` / `exit_code` / `error` - not usage, cost, duration or turns - and the
Run log is opened with `.truncate(true)` (`src/engine/runtime.rs:403-411`), so
**each Run overwrites the previous Run's cost data**. Only the most recent Run
per Worker survives anywhere in the system.

There is therefore no per-Run, per-change or per-batch cost history, and no
durable run identity to hang one on beyond bug-hunt ticket 07's run identity.

**This blocks ticket 07.** The pool cost/usage view aggregates "the per-Run costs
jjfx already records" - and it records none of them durably, so as written 07 has
no data source. 07 is marked `ready-for-agent` with `Blocked by: none`; it should
be blocked by this.

The cost of not having it is not hypothetical. A 24-ticket batch run through
parallel agents in October 2026 spent **65% of its ~$174 budget on review
fan-out** - 642 of 901 agent sessions - and one work stream looped **18 review
passes** without converging. Neither fact was visible while it was happening, or
from any tool afterwards: both had to be reconstructed by hand with recursive SQL
against a *different* store (the agent runtime's own session database). jjfx had
the same information in `RunUsage` and discarded it at finalization.

## Solution

Persist a durable, append-only per-Run record at finalization.

- Write the record where the Run is finalized (`Reservation::finalize`,
  `src/engine/pool.rs:224-247`), alongside the existing jjfx-owned state
  (`sessions.json`, `src/engine/state.rs:325-330`). An append-only JSONL is the
  natural shape: it is the same idiom the agent hooks already use
  (`src/hooks.rs:19-40`), it survives concurrent Workers, and it never truncates.
- Record, per Run: a run identity, the Worker, the workspace and its change,
  the runtime and model, cost, usage (input / cached / cache-write / output /
  reasoning), duration, turns, and the conclusion. This is the set
  `RunResult` already holds, so nothing new needs parsing.
- Treat an unrepresentable or missing cost the way bug-hunt ticket 28 handles it
  - a Run whose cost cannot be parsed must not break the record or aggregation.
- Keep the record readable without the TUI (it is the input to 07's view and to
  anything else that needs history).

Do **not** change what the Pool view shows today; that is 07's job. This ticket
is the data layer only.

## Commits

1. Define the durable per-Run record type and its append-only store, next to the
   existing jjfx-owned state, with the write at finalization.
2. Populate it from `RunResult` (cost, usage, duration, turns, conclusion, run
   identity, Worker, workspace/change).
3. Handle the unrepresentable-cost case per bug-hunt ticket 28.
4. Tests (a completed Run appends one record; a failed or missing-terminal Run
   still appends one; concurrent Workers do not interleave a partial line); docs
   (`docs/`); changelog if user-visible.

## Acceptance Criteria

- [x] Completing a Run appends exactly one durable record carrying its cost,
      usage, duration, turns and conclusion.
- [x] Two consecutive Runs in the same Worker both survive; the second does not
      overwrite the first.
- [x] A Run with no parseable cost still appends a record and does not break
      aggregation.
- [x] The store is append-only and never truncated.
- [ ] `mise run check` is green.

## Out of Scope

- The pool cost/usage **view** and the spend cap - that is ticket 07, which this
  unblocks.
- Provider usage/cost API reconciliation.
- Retention policy or pruning of the record.

## References

- `src/engine/run_log.rs:1519-1583` (`RunUsage`), `:1799-1813` (`RunCost`),
  `:1827-1910` (`RunResult`)
- `src/engine/pool.rs:1529-1544` (`apply_run_result`), `:224-247`
  (`Reservation::finalize`)
- `src/engine/runtime.rs:403-411` (truncate), `src/engine/state.rs:325-330`
- `.scratch/bug-hunt/issues/07-durable-run-identity.md`,
  `.scratch/bug-hunt/issues/28-misc-low-severity-fixes.md`
- `.scratch/opinionated-loop/issues/07-pool-cost-budget-view.md` (blocked by this)

## Comments

- 2026-10-15: Filed from the strategy review. Sits underneath 07 and was found
  while checking 07's premise: the costs it aggregates are not persisted, so 07
  cannot be built as written.
- 2026-10-15: Landed. `RunRecord` (`state.rs`) is the jjfx-owned document for one
  completed Run, kept in the append-only `.jj/pool/runs.jsonl` beside
  `sessions.json`; `RunRecordRepository` appends one JSON line under its own
  sidecar lock and never truncates, and a line that cannot be decoded is skipped
  so one torn tail never hides the rest. `Reservation::finalize` (`pool.rs`)
  writes the record once the Worker commit applies, populated from the
  `RunResult` jjfx already computes plus the Run-owned Worker state - identity
  (Worker and start timestamp), Ticket, workspace and bookmark, runtime and
  model, cost, usage, duration, turns and conclusion. A stale finalization
  appends nothing. An unusable or missing provider cost is recorded as absent,
  per bug-hunt ticket 28, so a Run with no usable cost still appends a record.
  Pinned by 5 store unit tests and 5 finalization unit tests (one record per
  completed Run; a failed Run and a Run with no cost still record; two
  consecutive Runs in one Worker both survive; a stale finalization appends
  nothing; concurrent appends never interleave). Documented in
  `docs/run-records.md`. The Pool view is deliberately unchanged - that is 07.
  One deliberate decision: the record carries the Worker's bookmark (`branch`)
  as the Run's change rather than reading the jj change id at finalization, so
  finalization stays off the jj read path and cannot fail on it.

## Resolution

Reconciled Mon 19 Oct 2026 against the merged code at `9d6bd0ce6e7b`.

This ticket was marked resolved without its acceptance boxes ticked. Every criterion was re-verified by an independent read-only review of the code and tests on `main`. The reconciliation recorded no new work; it confirms what shipped.

Evidence: `RunRecord` (`src/engine/state.rs:449`) is the jjfx-owned document for one completed Run, kept in the append-only `runs.jsonl` (`:44`, `:437`) beside `sessions.json`. `RunRecordRepository` (`:969`, `:2384`) appends one JSON line under its own sidecar lock via `append` (`:2391`) and never truncates; `records` (`:2424`) decodes them, skipping a line that cannot be decoded so one torn tail never hides the rest. `Reservation::finalize` (`src/engine/pool.rs`) writes the record once the Worker commit applies, populated from the `RunResult` plus the Run-owned Worker state. The ticket's own comment records the coverage: 5 store unit tests and 5 finalization unit tests - one record per completed Run; a failed Run and a Run with no cost still record; two consecutive Runs in one Worker both survive; a stale finalization appends nothing; concurrent appends never interleave. An unusable or missing provider cost is recorded as absent, per bug-hunt 28, so criterion 3 holds. Documented in `docs/run-records.md`.

Not ticked: the `mise run check` criterion. It is a landing-commit property, and this reconciliation deliberately does not run the full suite (the orchestrator runs the gate once at the end).
