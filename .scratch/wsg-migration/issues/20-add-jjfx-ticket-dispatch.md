# Add Ticket Dispatch workflows to jjfx

Status: resolved

## Parent

issues/16-integrate-dispatch-into-jjfx.md

## Problem Statement

The shared library can reserve Workers and launch Direct Dispatch, but jjfx has
no Ticket input or interactive routing workflow.

## Solution

Build on issue 19's Workspace Dispatch command/event controller to add explicit
selected-Worker Direct Dispatch, bulk Dispatch, Ready Ticket discovery, typed
capacity confirmation, ordered outcomes, and responsive background execution.

## Commits

1. Add Ticket input for selected-Worker Direct Dispatch.
2. Add bulk and Ready Ticket discovery and preview.
3. Add capacity growth, partial dispatch, cancellation, and ordered outcomes.
4. Add message, rendering, error, and non-blocking interaction tests.

## Acceptance Criteria

- [x] A Ticket can be dispatched explicitly to the selected idle Worker.
- [x] Bulk and Ready Ticket flows use shared typed interfaces.
- [x] Capacity shortages produce the approved confirmation flow.
- [x] Dispatch outcomes remain ordered and visible.
- [x] Input remains responsive during preparation and launch.
- [x] `mise run check` is green.

## Out of Scope

- Dispatch Group progress and logs
- Worker Session actions
- Shared wsg/jjfx startup

## Blocked by

- issues/19-enable-writable-worker-pool-in-jjfx.md

## Answer

Implemented Ticket Dispatch as a deep extension of the existing Workspace
Dispatch controller seam. Pool mode now supports selected idle-Worker Dispatch,
ordered multi-Ticket input, Ready Ticket discovery through the configured Agent
Runtime, preview and cancellation, typed capacity shortage decisions, approved
Pool growth, use-available partial Dispatch, and immutable ordered outcomes.

All repository access, Ticket discovery, request construction, Worker
Reservations, capacity policy, and Agent Runtime launch remain behind the
controller adapter. App owns only interaction modes and presentation state, so
background preparation and launch do not block the TUI event loop. Worker Pool
state refreshes after successful Dispatch and stale operation identities remain
ignored.

jjfx is version 0.31.0 with the user-visible change recorded in `CHANGELOG.md`.
`mise run check` passes across jjfx, wsg, and wsg-core.

## Comments

2026-08-06 - Completed the four TDD slices and verified the full workspace gate.
The first full check encountered one unrelated parallel-test temporary-directory
collision; the focused test passed and the complete gate passed on rerun.
