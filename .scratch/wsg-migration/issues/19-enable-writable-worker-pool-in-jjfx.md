# Enable writable Worker Pool management in jjfx

Status: ready-for-agent

## Parent

issues/16-integrate-dispatch-into-jjfx.md

## Problem Statement

jjfx currently reads Go-created Worker Pool state but marks it READ-ONLY and
cannot manage capacity. Writable behavior must not leak blocking persistence,
locking, or Worker lifecycle rules into App.

## Solution

Introduce one deep Workspace Dispatch command/event controller seam between App
and `wsg-core`. The real adapter owns repository access and blocking operations;
the test adapter records commands and emits deterministic events. App owns only
immutable Pool presentation state and interaction modes.

Remove the obsolete `MigrationCapabilities` read-only marker. Carry optional
Worker metadata through the Workspace presentation rows, preserving the separate
Attention, Agent, Work, Forge, and Worker Status lifecycles.

Add a focused Pool management mode supporting exact capacity entry, growth,
confirmed shrink, confirmed destruction, stable Worker selection, diagnostics,
operation correlation, and responsive background execution.

## Commits

1. Add the failing controller command/event contract test, then implement the
   real and in-memory adapters.
2. Replace the read-only Worker Pool poller with controller refresh events and
   preserve the last valid snapshot while surfacing refresh failures.
3. Carry optional Worker presentation metadata through Workspace rows and remove
   the obsolete capability and READ-ONLY markers.
4. Add Pool management mode with exact capacity entry, growth, shrink and
   destruction confirmations, typed outcomes, and refresh-after-mutation.
5. Add App and rendering coverage for refresh during input, stable selection,
   stale operation results, cancellation, diagnostics, narrow terminals, and
   preservation of existing lifecycle behavior.
6. Update the jjfx version and changelog after behavior is complete.

## Acceptance Criteria

- [ ] The command/event controller is the only App seam for Pool operations.
- [ ] Blocking Pool reads and mutations never run in the App event loop.
- [ ] App retains the last valid snapshot when refresh fails and shows the error.
- [ ] jjfx manages existing and Rust-created Pools with exact capacity changes.
- [ ] Shrink and destruction require explicit confirmation.
- [ ] Worker metadata is presentation data, not a second mutable Pool model.
- [ ] No read-only capability or READ-ONLY UI marker remains.
- [ ] Existing lifecycle, selection, help, and rendering behavior remains intact.
- [ ] jjfx is version 0.30.0 and the changelog records the user-visible change.
- [ ] `mise run check` is green.

## Out of Scope

- Ticket Dispatch and orchestration UI
- Structured logs and Worker Session actions
- Shared wsg/jjfx TUI startup
- Persisted schema changes

## Blocked by

- issues/04-import-worker-pool-snapshots.md
- issues/09-port-logs-sessions-and-worker-actions.md
- issues/11-port-direct-dispatch.md
- issues/13-port-orchestration-runner.md

## Comments

2026-08-06 - Split from issue 16 as the first vertical implementation slice.
