# Port Worker Workspace and Worker Pool lifecycle

Status: ready-for-agent

## Parent

epics/B-worker-pool-and-runtime.md

## Problem Statement

jjfx can create ad hoc Workspaces, but it does not own reusable Worker identities, Worker Workspace provisioning, pool capacity, Reservations, or teardown. Reimplementing those as frontend commands would duplicate rules and risk divergence between jjfx and wsg.

## Solution

Add a Worker Pool aggregate to the shared library and converge its Workspace operations with jjfx's existing Store. The aggregate owns creation, growth, shrinking, named removal, reset preparation, destruction, aliases, and Reservations while delegating persistent I/O to the state repositories.

## Commits

1. Introduce Worker ID, Worker alias, pool capacity, and Reservation value types.
2. Add Worker Workspace provisioning over the shared Repository and ws-cache behavior.
3. Add rollback when jj Workspace creation, environment copying, or cache projection fails.
4. Implement pool creation and growth with stable Worker IDs.
5. Implement atomic Reservation of the first idle Workers.
6. Implement Reservation of a named idle Worker.
7. Implement shrink and named removal with busy-Worker protection.
8. Implement pool destruction with complete Workspace and state cleanup.
9. Implement aliases as cosmetic pool metadata that survives Worker reset.
10. Reuse the shared Workspace operations from jjfx without changing ad hoc Workspace behavior.

## Decision Document

- The shared Repository owns Workspace provisioning used by both frontends.
- Ad Hoc Workspaces and Worker Workspaces remain distinct roles.
- Worker IDs are stable and aliases never alter paths or jj names.
- Reservation changes capacity ownership but does not launch a Run.
- Pool mutation is serialized through the pool repository.
- Reset process termination belongs to Run supervision; this ticket provides only lifecycle preparation and state transitions that do not require a live process.

## Testing Decisions

Test public pool operations in temporary jj repositories. Cover partial provisioning failure, concurrent Reservations, insufficient capacity, busy shrink rejection, alias persistence, teardown, and ws-cache compatibility. Existing Workspace Store tests are prior art and should remain green.

## Acceptance Criteria

- [ ] Rust-created Worker Pools are usable by Go wsg.
- [ ] Existing Go-created pools can grow and shrink through Rust.
- [ ] Concurrent Reservations never allocate one Worker twice.
- [ ] Failed provisioning leaves no registered half-Workspace or claimed Worker.
- [ ] Ad Hoc Workspace behavior remains unchanged.
- [ ] `mise run check` is green.

## Out of Scope

- Agent Runtime launch
- Ticket discovery
- Dispatch Group progression
- TUI controls

## Blocked by

- issues/06-port-state-persistence-and-locking.md
