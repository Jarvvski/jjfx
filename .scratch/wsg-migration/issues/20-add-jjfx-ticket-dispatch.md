# Add Ticket Dispatch workflows to jjfx

Status: claimed

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

- [ ] A Ticket can be dispatched explicitly to the selected idle Worker.
- [ ] Bulk and Ready Ticket flows use shared typed interfaces.
- [ ] Capacity shortages produce the approved confirmation flow.
- [ ] Dispatch outcomes remain ordered and visible.
- [ ] Input remains responsive during preparation and launch.
- [ ] `mise run check` is green.

## Out of Scope

- Dispatch Group progress and logs
- Worker Session actions
- Shared wsg/jjfx startup

## Blocked by

- issues/19-enable-writable-worker-pool-in-jjfx.md
