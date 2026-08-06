# Restore wsg Dispatch and Agent Session CLI compatibility

Status: resolved

## Parent

epics/D-interfaces-and-cutover.md

## Problem Statement

Users still depend on wsg for Direct Dispatch, bulk Ticket routing, Parent Ticket orchestration, Follow-ups, review handling, logs, Mount, and shell completion. Until those commands are available from Rust, the Go binary remains operationally required.

## Solution

Add thin CLI adapters over Direct Dispatch, orchestration, Worker actions, and Agent Session outcomes. Preserve existing options, aliases, pool-growth confirmation, foreground behavior, summaries, internal detached entrypoints, and generated shell completion.

## Commits

1. Restore Direct Dispatch parsing for one or more Tickets, foreground mode, model, budget, and orchestration controls.
2. Restore Ready Ticket bulk discovery with label filtering.
3. Render typed capacity shortages as the existing resize confirmation flow.
4. Render ordered per-Ticket Direct Dispatch outcomes and foreground completion.
5. Restore Parent Ticket orchestration and its detached internal command.
6. Restore Send with visible resumed-versus-fresh outcome.
7. Restore Review, Logs, Mount, Rebase, Open PR, Reset, and related aliases.
8. Restore zsh completion and internal dynamic completion over live pool snapshots.
9. Restore default command help behavior outside a TTY or without an active pool.
10. Add black-box command tests for success, partial failure, cancellation, and malformed state.

## Decision Document

- The CLI translates flags into typed requests and renders typed outcomes.
- Pool growth confirmation is never performed inside the shared library.
- Internal orchestration commands remain undocumented compatibility details.
- Shell completion reads reconciled snapshots and never mutates state.
- Running wsg with no arguments in a usable TTY will enter the jjfx TUI after ticket 16; this ticket preserves safe fallback behavior first.

## Testing Decisions

Test binary behavior with fake Agent Runtime executables, temporary Repositories, and deterministic Ticket discovery adapters. Assert stdout/stderr separation, exit outcomes, aliases, completion candidates, and foreground versus background behavior. Reuse shared-library tests instead of retesting private transitions.

## Acceptance Criteria

- [x] Every command and option in the compatibility inventory has a Rust path.
- [x] Dispatch resize prompts report the actual locked capacity gap.
- [x] Follow-up visibly reports resumed or fresh Session behavior.
- [x] Completion reconciles dead busy Workers.
- [x] Parent Ticket orchestration can run detached and resume.
- [x] `mise run check` is green.

## Out of Scope

- Recreating Bubble Tea
- New CLI commands
- Final binary replacement
- Deprecating the Go repository

## Blocked by

- issues/13-port-orchestration-runner.md
- issues/14-restore-wsg-workspace-and-pool-cli.md

## Comments

2026-08-06 - Implemented the approved Rust compatibility surface in focused
TDD slices. The wsg binary now supports strict Dispatch parsing and budget
controls, Ready Ticket bulk discovery, exact locked capacity prompts, ordered
foreground/background outcomes, detached orchestration preparation and resume,
Follow-up Session reporting, Review, Logs, Mount, Rebase, Open PR, Reset,
zsh completion, live reconciled Worker candidates, and safe no-argument help.

Verification: `mise run check` passed. The wsg package version is 0.9.0 and the
existing jjfx package remains 0.29.0.
