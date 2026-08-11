# Integrate Worker Pool and Dispatch into jjfx

Status: resolved

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

- [x] jjfx manages existing and Rust-created Worker Pools.
- [x] Direct and orchestrated Dispatch run without blocking input rendering.
- [x] Logs, Send, Review, Reset, and aliases work from the TUI.
- [x] Existing Attention, Agent, Work, Forge, and Workspace flows remain intact.
- [x] `wsg` with no arguments enters the same TUI when appropriate.
- [x] `mise run check` is green.

## Child tickets

The implementation is split into five vertical tickets so each can be completed
with red-green TDD, focused review, and the repository verification gate:

1. `issues/19-enable-writable-worker-pool-in-jjfx.md` - writable Pool presentation,
   the Workspace Dispatch command/event controller, and Pool management.
2. `issues/20-add-jjfx-ticket-dispatch.md` - selected-Worker, bulk, and Ready Ticket
   Direct Dispatch with capacity confirmation.
3. `issues/21-show-dispatch-progress-and-logs.md` - Dispatch Group progress, waves,
   and structured log detail.
4. `issues/22-add-jjfx-worker-session-actions.md` - Send, Review, Session outcomes,
   Reset, Rebase, Open PR, aliases, and dismiss behavior.
5. `issues/23-share-and-harden-jjfx-tui.md` - shared jjfx/wsg startup, help,
   narrow layouts, PTY coverage, and end-to-end hardening.

All five child tickets are now resolved. They preserve the command/event seam
introduced by issue 19 and complete the integrated Worker Pool and Dispatch TUI.

## Comments

2026-08-06 - Issue 19 is resolved. It introduced the Workspace Dispatch
command/event seam, writable Pool management, stable Worker presentation data,
and the version 0.30.0 jjfx integration.

2026-08-06 - Issue 20 is resolved. jjfx 0.31.0 now supports selected-Worker,
bulk, and Ready Ticket Dispatch with previews, typed capacity decisions,
ordered outcomes, and responsive background execution. Issue 21 is now the
successive implementation frontier for Dispatch Group progress and logs.

2026-08-12 - Issue 23 is resolved. jjfx 0.34.0 and wsg 0.10.0 now share one
interactive TUI launcher, contextual help, narrow-layout priorities, PTY
terminal restoration coverage, and active Worker-mode message/render tests.
The integrated Worker Pool and Dispatch TUI is complete and issue 17 is
unblocked.

## Answer

All five child tickets are resolved. The integrated jjfx TUI now manages the
compatible Worker Pool and Dispatch lifecycle through the shared command/event
seam, while preserving the existing Attention, Agent, Work, Forge, and
Workspace lifecycles. The final child also made the interactive TUI reusable by
`wsg`, hardened terminal restoration, and added PTY and active-mode coverage.

`mise run check` passes with jjfx 0.34.0 and wsg 0.10.0. This completes the
integrated Worker Pool and Dispatch TUI and unblocks parity and release work in
issue 17.

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
