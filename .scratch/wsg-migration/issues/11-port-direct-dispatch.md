# Port Reservations and Direct Dispatch

Status: ready-for-agent

## Parent

epics/C-dispatch-and-orchestration.md

## Problem Statement

A safe Direct Dispatch crosses several failure-prone modules: capacity must be reserved atomically, the Worker Workspace must be prepared, Repository identity must be resolved, prompts must be built, and the Agent Runtime must launch. Failure at any point must not strand a busy Worker or expose a Workspace while it is being reset.

## Solution

Add one shared Direct Dispatch operation that coordinates Worker Pool Reservation, optional growth, Workspace preparation, prompt construction, and Run launch. Support first-idle and explicitly selected Workers, foreground and background modes, and bulk input while returning typed per-Ticket outcomes.

## Commits

1. Define a Dispatch request and outcome that carries Ticket, model, budget, foreground mode, and dependency context.
2. Implement atomic Reservation of enough idle Workers for a bulk request.
3. Return a typed capacity shortage before prompting or growing.
4. Implement caller-approved growth and Reservation in one locked operation.
5. Implement explicit named-Worker Direct Dispatch with idle validation.
6. Prepare each Worker Workspace on the requested base before launch.
7. Resolve Repository identity and build the initial prompt.
8. Launch the Run and return Worker, Ticket, PID, and foreground outcome.
9. Release every Reservation whose preparation or launch fails.
10. Add partial-claim behavior only for the existing command paths that intentionally allow it.

## Decision Document

- Reservation and Dispatch are distinct operations but coordinated behind one deep interface for normal callers.
- A Reservation marks capacity busy before expensive discovery or preparation when immediate UI feedback requires it.
- Pool growth requires a frontend decision; the library reports the exact gap.
- Explicit Worker selection never falls back silently to another Worker.
- Bulk outcome preserves input Ticket order.
- A failed pre-launch Dispatch returns the Worker to Idle.

## Testing Decisions

Test Direct Dispatch against fake Workspace, prompt, and Run adapters plus real state repositories. Cover concurrent claims, pool shortage, rejected growth, failed preparation, failed identity lookup, failed launch, foreground behavior, explicit selection, and ordered bulk outcomes.

## Acceptance Criteria

- [ ] Concurrent Direct Dispatch never double-assigns a Worker.
- [ ] Every pre-launch failure releases its Reservation.
- [ ] Explicit Worker selection is deterministic.
- [ ] Bulk Dispatch reports success and failure per Ticket in input order.
- [ ] Go wsg observes compatible busy state and can reconcile a Rust-launched Run.
- [ ] `mise run check` is green.

## Out of Scope

- Parent Ticket orchestration
- Automatic dependency graph discovery
- CLI resize prompts
- TUI controls

## Blocked by

- issues/07-port-workspace-and-pool-lifecycle.md
- issues/09-port-logs-sessions-and-worker-actions.md
- issues/10-port-linear-discovery-and-prompts.md
