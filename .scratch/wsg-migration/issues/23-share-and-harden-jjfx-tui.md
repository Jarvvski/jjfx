# Share and harden the jjfx TUI across jjfx and wsg

Status: resolved

## Parent

issues/16-integrate-dispatch-into-jjfx.md

## Problem Statement

The jjfx TUI entrypoint is private to the jjfx binary, while wsg's no-argument
path does not enter the same interface. The integrated UI also needs final
help, narrow-layout, terminal-restoration, and end-to-end coverage.

## Solution

Extract one reusable jjfx TUI launcher for both binaries. Preserve headless and
version behavior, restore the terminal on success and panic, update the help
source of truth and narrow layouts, and add PTY and end-to-end message tests.

## Commits

1. Extract and reuse the common jjfx TUI startup path from both binaries.
2. Update help, keybindings, and narrow-terminal priority rules.
3. Add PTY smoke coverage for both binary names and terminal restoration.
4. Add end-to-end message/rendering coverage during active Worker modes.

## Acceptance Criteria

- [x] `jjfx` and appropriate no-argument `wsg` startup enter the same TUI.
- [x] Version, hooks, and non-TTY command behavior remain intact.
- [x] Terminal restoration works on quit and panic paths.
- [x] Narrow layouts preserve primary lifecycle and Worker identity information.
- [x] PTY and end-to-end tests pass.
- [x] `mise run check` is green.

## Out of Scope

- Final release parity and installation cutover
- Go repository deprecation
- Persisted schema redesign

## Answer

Implemented the final jjfx TUI integration slice in four focused commits.
The root `jjfx` library now owns one deep interactive launcher reused by the
`jjfx` and interactive no-argument `wsg` adapters. Explicit version, hooks,
headless, completion, and non-TTY command behavior remain in their existing
binary-specific paths.

Normal and Worker Pool help use contextual binding tables, Pool help preserves
its prior interaction state, and narrow Workspace and Pool rows prioritize
Worker identity, lifecycle status, and active Ticket information. The terminal
session now restores transactionally on setup failure, normal quit, and panic;
PTY coverage exercises both binary names and all restoration paths. App message
and rendering tests cover Pool refreshes while Ticket input is active.

`jjfx` is version 0.34.0, `wsg` is version 0.10.0, and `mise run check` passes.

## Blocked by

- issues/22-add-jjfx-worker-session-actions.md
