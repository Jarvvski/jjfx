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
- [x] Concurrent Reservations never allocate one Worker twice.
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

## Answer

Partially implemented. Creation, growth, and Reservation are done; shrink, named
removal, pool destruction, and aliases are not. See the 2026-07-30 comment.

Implemented the pool creation and growth slice. The shared `WorkerPool` module now
creates compatible pool state, provisions stable random Worker IDs, preserves
existing membership and metadata while growing, rejects shrink requests,
serializes ws-cache projection, and compensates failed growth without deleting
state it no longer owns.

Added public-seam integration coverage for creation, growth, Go-created pools,
idempotence, failure, cache projection, and concurrent growth. `mise run check`
passes.

## Comments

2026-07-27 - Completed the missing Reservation slice through the public
`WorkerPool` interface. First-idle and named Reservations now atomically mark
Workers busy under the compatible pool and Worker locks, preserve Go wire
fields and unknown extensions, reject unavailable Workers without mutation, and
serialize concurrent claims without duplicate allocation. Focused Worker Pool
tests pass.

2026-07-30 - Reopened during a tracker audit: this ticket was marked resolved
while half its scope was still unbuilt. Commits 1 through 6 landed, but the
shared library has no operation for commits 7, 8, or 9. `WorkerPool`'s only
mutating entry points are `grow_to`, `reserve`, `reserve_named`, and
`reconcile_runs`; `grow_to` explicitly rejects a lower capacity with
`CannotShrink`, and no shrink, named-removal, destroy, or alias operation exists
anywhere in `crates/wsg-core/src/`. Five of the six acceptance criteria are
correspondingly unmet, which is why they were never checked.

Remaining work: shrink and named removal with busy-Worker protection, pool
destruction with complete Workspace and state cleanup, aliases as cosmetic pool
metadata that survives a Worker reset, and reusing the shared Workspace
operations from jjfx without changing ad hoc Workspace behavior. Epic B's
definition of done depends on these ("create, grow, shrink, reset, and destroy"),
and ticket 14 cannot restore `wsg pool resize`, named remove, or destroy as a
thin CLI over a library that lacks them.

This does not invalidate ticket 08. Run supervision only consumed Workspace
provisioning, pool growth, and Reservation, all of which are complete; the
missing operations are pool-membership teardown and cosmetic metadata, which
Run supervision never touched.
