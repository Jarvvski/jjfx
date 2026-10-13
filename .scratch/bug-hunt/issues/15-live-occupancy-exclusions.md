# Make Dispatch Eligibility a live query, not a frozen snapshot

Status: resolved
Type: task
Blocked by: none

## Problem

Occupancy exclusions were computed once and frozen for the whole orchestration
run. The adapter built a static `DispatchExclusions`
(`workspace_dispatch/adapter.rs`) and the runner held it, applying the same
snapshot on every claim. A Workspace opened in a terminal (or closed) after
orchestration started was therefore missed: the runner could dispatch into a tab
a human had since opened, and never noticed capacity freed by closing a tab.

That deviates from ADR-0011, which requires the frontend to reconcile the in-use
marker against **live** terminal state.

## Fix

A new `ExclusionsProvider` (`engine/eligibility.rs`) wraps a closure that
recomputes the exclusion set. The runner and `LiveExecution` hold a provider, and
`claim` calls it **per claim**:

- `OrchestrationRunner::with_exclusions` now takes an `ExclusionsProvider`.
- `LiveExecution::current_exclusions` re-queries it before each
  `direct_dispatch().reserve(..)`, so a newly occupied Worker is excluded from
  the very next decision and a freed one becomes available again.
- `discover`'s placeholder reservation also consults the provider.
- `ExclusionsProvider::fixed(..)` preserves the snapshot behaviour for callers
  that only have a set.

The engine stays frontend-neutral: both callers still compute the set. The
adapter builds the provider from its terminal seam (`live_exclusions`, shared
with the non-orchestration path), and the CLI builds one from its
`open_workspaces` function pointer. Both still **fail closed** when the terminal
cannot be queried.

## Commits

1. `Re-query Dispatch occupancy on every claim` - the provider, the runner and
   `LiveExecution` wiring, the adapter and CLI providers, two tests, and a
   CHANGELOG `(fix)`.

## Testing Decisions

Claiming only *reserves* - it never launches - so `LiveExecution` can be
exercised against a real Repository with no Agent Runtime:

- `live_execution_re_queries_occupancy_on_every_claim` - a provider that excludes
  nothing on the first call and both Workers on the second. The first claim takes
  `worker-01`; the second returns `None`, and the provider is consulted exactly
  twice.
- `a_fixed_provider_keeps_the_opening_snapshot` - the contrast case, asserting
  that a fixed provider still reserves both Workers. This is precisely the old
  frozen-snapshot behaviour, so the pair demonstrates the change.

## Acceptance Criteria

- [x] A Workspace opened after orchestration starts is not dispatched to.
- [x] A Workspace closed after orchestration starts becomes available.
- [x] The engine never inspects the terminal itself (ADR-0011).
- [x] `mise run check` is green.

## Out of Scope

- Changing the occupancy marker format.
