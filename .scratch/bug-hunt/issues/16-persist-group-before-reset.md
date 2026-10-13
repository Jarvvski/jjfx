# Persist the Dispatch Group before resetting the Worker

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

The retry path and the Done path order their durable writes oppositely:

- Done (`orchestration.rs:448-464`): save the group at `:457`, reset the Worker
  at `:463`.
- Retry (`:465-482`): reset the Worker at `:466`, then `apply`/`save` at
  `:467-476`.

If `save_group` fails (e.g. `ConcurrentChange`) after `reset_worker` succeeded,
the Worker is idle with its ticket cleared but the persisted group still says
`dispatched`. On restart, reconciliation (`:430-447`) sees
`observation.ticket != ticket` and errors `WorkerTicketMismatch`, so the parent
can never be resumed.

## Fix

Standardise on **persist-before-side-effect** for every terminal transition:
commit the group first, then mutate the Worker (as the Done path already does).

## Commits

1. Reorder the retry path to save before reset.
2. Test: a save conflict leaves the Worker and group consistent (resumable).
3. CHANGELOG entry `(fix)`.

## Acceptance Criteria

- [ ] A `save_group` conflict on the retry path leaves a resumable state.
- [ ] No terminal transition mutates the Worker before persisting the group.
- [ ] `mise run check` is green.

## Out of Scope

- The failed-dependency liveness fix (ticket 04).
