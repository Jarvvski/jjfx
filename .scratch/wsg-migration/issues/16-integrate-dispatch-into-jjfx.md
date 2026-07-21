# Integrate Worker Pool and Dispatch into jjfx

Status: ready-for-agent

## Parent

epics/D-interfaces-and-cutover.md

## Problem Statement

The shared Rust implementation delivers its main architectural benefit only when jjfx can operate it directly. A naive integration could overload the existing Agent and Work lifecycles, duplicate library state in App, block the event loop, or recreate wsg's Bubble Tea interface alongside the existing Attention-first design.

## Solution

Extend jjfx's Ratatui application with Worker Pool and Dispatch messages, views, and actions over shared typed interfaces. Worker metadata augments existing Workspace rows. Long operations run outside the App owner and return messages. Add Ticket input, pool capacity management, Direct Dispatch, orchestration progress, logs, Send, Review, Reset, and Worker naming without replacing the existing Attention model.

## Commits

1. Replace the read-only migration badge with stable Worker metadata in the Workspace presentation model.
2. Add Worker Pool snapshot changes to App messages without storing a second mutable domain model.
3. Add a focused pool capacity and Worker management mode.
4. Add Ticket input for explicit selected-Worker Direct Dispatch.
5. Add bulk and Ready Ticket Dispatch flows with capacity confirmation.
6. Show Dispatch Group progress and dependency wave status.
7. Add live structured log detail using the provider-neutral event model.
8. Add Send and Review editors with visible Session resume outcomes.
9. Add Reset, Rebase, Open PR, alias, and dismiss actions with safe confirmation where destructive.
10. Route no-argument wsg startup into the same jjfx TUI entrypoint.
11. Update help, keybinding source of truth, and narrow-terminal layouts.
12. Add end-to-end message and rendering tests for pool changes during active user modes.

## Decision Document

- jjfx remains the only Rust TUI.
- App owns presentation state, not a mutable copy of Worker Pool rules.
- Shared operations execute asynchronously and fold typed outcomes through messages.
- Worker Status does not replace Agent lifecycle, Work lifecycle, or Attention.
- Destructive Reset remains visually distinct and confirmed.
- wsg no-argument behavior reuses jjfx rather than maintaining a second frontend.

## Testing Decisions

Use existing App tests and fake shared-library adapters. Cover state refresh while editing, selection stability, narrow screens, errors, confirmation, concurrent progress events, and terminal restoration. Add a PTY smoke test for launching the shared TUI from both binary names.

## Acceptance Criteria

- [ ] jjfx manages existing and Rust-created Worker Pools.
- [ ] Direct and orchestrated Dispatch run without blocking input rendering.
- [ ] Logs, Send, Review, Reset, and aliases work from the TUI.
- [ ] Existing Attention, Agent, Work, Forge, and Workspace flows remain intact.
- [ ] `wsg` with no arguments enters the same TUI when appropriate.
- [ ] `mise run check` is green.

## Out of Scope

- Porting Bubble Tea widgets or keybindings exactly
- Changing persisted schemas
- Replacing the existing Attention model
- Final release cutover

## Blocked by

- issues/04-import-worker-pool-snapshots.md
- issues/09-port-logs-sessions-and-worker-actions.md
- issues/11-port-direct-dispatch.md
- issues/13-port-orchestration-runner.md
