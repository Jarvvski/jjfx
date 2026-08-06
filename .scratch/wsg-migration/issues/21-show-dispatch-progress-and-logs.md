# Show Dispatch progress and structured logs in jjfx

Status: ready-for-agent

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

- [ ] Dispatch Group progress and dependency waves are visible.
- [ ] Orchestration events do not block input rendering.
- [ ] Structured Claude and Codex activity renders through shared values.
- [ ] Terminal and malformed log outcomes remain understandable.
- [ ] `mise run check` is green.

## Out of Scope

- New persisted schemas
- Send, Review, Reset, or other Worker actions
- Shared wsg/jjfx startup

## Blocked by

- issues/20-add-jjfx-ticket-dispatch.md
