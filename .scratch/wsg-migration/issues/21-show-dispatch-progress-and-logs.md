# Show Dispatch progress and structured logs in jjfx

Status: resolved

## Parent

issues/16-integrate-dispatch-into-jjfx.md

## Problem Statement

Dispatch Group execution and provider-neutral Run activity are available in the
shared library but invisible in jjfx.

## Solution

Use the Workspace Dispatch controller to stream orchestration events, display
Dispatch Group status and dependency waves, and show structured Run activity in
a focused detail view without blocking the event loop.

## Commits

1. Add immutable Dispatch Group progress presentation state.
2. Stream orchestration events and show dependency wave status.
3. Add structured log polling or tailing with provider-neutral events.
4. Cover refresh, malformed records, terminal progress, narrow layouts, and
   concurrent updates.

## Acceptance Criteria

- [x] Dispatch Group progress and dependency waves are visible.
- [x] Orchestration events do not block input rendering.
- [x] Structured Claude and Codex activity renders through shared values.
- [x] Terminal and malformed log outcomes remain understandable.
- [x] `mise run check` is green.

## Out of Scope

- New persisted schemas
- Send, Review, Reset, or other Worker actions
- Shared wsg/jjfx startup

## Blocked by

- issues/20-add-jjfx-ticket-dispatch.md

## Answer

Implemented Dispatch Group progress and focused Worker logs through the
Workspace Dispatch controller seam. Pool mode now has a separate `o` Parent
Ticket orchestration flow while preserving selected-Worker Direct Dispatch on
`d`. Immutable presentation values expose dependency waves, blockers, ready
Tickets, Worker assignments, retries, status counts, and terminal group state.

The controller streams start, progress, direct-dispatch, terminal, and failure
events without blocking the Ratatui event loop. Focused Worker detail mode
uses a cancellable, generation-correlated watcher over the shared `RunLog`
interface and renders provider-neutral Claude and Codex activity, usage,
terminal results, malformed records, and unavailable-log errors. Stale updates,
removed Workers, terminal groups, concurrent refreshes, and narrow layouts are
covered by tests without adding persistence schemas or mutable domain state to
`App`.

jjfx is version 0.32.0 with the user-visible change recorded in
`CHANGELOG.md`. `mise run check` passes across jjfx, wsg, and wsg-core.
