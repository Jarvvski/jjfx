# Terminate a Dispatch Group when a dependency fails

Status: ready-for-agent
Type: task
Blocked by: 03

## Problem

A terminally `Failed` Sub-issue permanently blocks every Sub-issue that lists it
as a Blocker, so the group never reaches a terminal state and
`OrchestrationRunner::run` polls forever at 5s while holding the orchestrate
flock. There is no cancellation path and no production `max_cycles`
(`OrchestrationOptions::new()` at `workspace_dispatch/adapter.rs:463` leaves it
unset), so the thread and lock leak until the process is killed.

- `SubIssueStatus::unblocks()` = `Done | Skipped`
  (`src/engine/dispatch_group.rs:55-57`).
- `is_terminal()` = `!is_active()` (`:50`).
- `run` (`src/engine/orchestration.rs:1064-1077`) loops with no liveness exit.

This violates `CONTEXT.md`'s Dependency ("must be completed before another
Sub-issue becomes eligible") and the group's own terminal contract.

## Fix

Model terminal failure explicitly in `dispatch_group.rs` and give the runner a
liveness exit:

1. A dependent whose Blocker is terminal-but-unsatisfied can never run. Decide
   the representation - an explicit unreachable/`Skipped { reason }` transition
   propagated from the failed blocker, or a `is_terminal()` that accounts for
   "no pending Sub-issue can ever become ready".
2. Give `OrchestrationOptions` a bounded default `max_cycles` and a distinct
   "no progress" terminal error so a stuck group surfaces instead of hanging.
3. Surface the outcome through `OrchestrationSummary` and the TUI/CLI.

## Commits

1. Add the unreachable/dependent-failure transition in `dispatch_group.rs` with
   unit tests (diamond and chain shapes).
2. Make `is_terminal()` account for it; make `run` return a summary instead of
   looping.
3. Add a bounded default `max_cycles` and a no-progress error; update the
   adapter and CLI.
4. CHANGELOG entry `(fix)`.

## Decision Document

- Choose cancel vs skip-with-reason vs fail-the-group here and record it; the
  map's Fog flags this as undecided.
- Keep the persisted Dispatch Group document Go-compatible: prefer an existing
  status value (`skipped` with a `skip_reason`) over a new one.

## Acceptance Criteria

- [ ] A group containing a failed blocker reaches a terminal state.
- [ ] `run` cannot loop forever: a no-progress group terminates with an error.
- [ ] The failure is visible to the operator (summary + TUI/CLI).
- [ ] The regression test from ticket 03 passes.
- [ ] `mise run check` is green.

## Out of Scope

- Retrying a failed Sub-issue automatically.
- Changing the frozen wire vocabulary.
