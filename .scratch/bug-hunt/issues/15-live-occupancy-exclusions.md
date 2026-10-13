# Make Dispatch Eligibility a live query, not a frozen snapshot

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

Occupancy exclusions are computed once and frozen for the whole orchestration
run: the adapter builds a static `DispatchExclusions`
(`workspace_dispatch/adapter.rs:414`, `:435-437`) and `LiveExecution` holds it
(`orchestration.rs:650-653`, `:815-819`, used at `:1008`, `:1058`). A Workspace
opened in a terminal (or closed) after orchestration starts is therefore missed:
the runner can dispatch into a tab a human has since opened, and never notices
capacity freed by closing a tab.

This deviates from ADR-0011, which requires the frontend to reconcile the
in-use marker against **live** terminal state ("Occupied is a view of live
terminal state").

## Fix

Pass an exclusions **provider** (a closure or a re-query per claim) into
`LiveExecution` instead of a snapshot, so each claim decision sees current
occupancy. Keep the engine frontend-neutral: the frontend still computes the
exclusion set; only its freshness changes.

## Commits

1. Replace the frozen `DispatchExclusions` in the orchestration path with a
   provider.
2. Re-query per claim (or per cycle) and honour it in `reserve`.
3. Test: a Workspace opened mid-run is not dispatched to.
4. CHANGELOG entry `(fix)`.

## Acceptance Criteria

- [ ] A Workspace opened after orchestration starts is not dispatched to.
- [ ] A Workspace closed after orchestration starts becomes available.
- [ ] The engine never inspects the terminal itself (ADR-0011).
- [ ] `mise run check` is green.

## Out of Scope

- Changing the occupancy marker format.
