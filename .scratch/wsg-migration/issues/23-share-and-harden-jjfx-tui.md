# Share and harden the jjfx TUI across jjfx and wsg

Status: ready-for-agent

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

- [ ] `jjfx` and appropriate no-argument `wsg` startup enter the same TUI.
- [ ] Version, hooks, and non-TTY command behavior remain intact.
- [ ] Terminal restoration works on quit and panic paths.
- [ ] Narrow layouts preserve primary lifecycle and Worker identity information.
- [ ] PTY and end-to-end tests pass.
- [ ] `mise run check` is green.

## Out of Scope

- Final release parity and installation cutover
- Go repository deprecation
- Persisted schema redesign

## Blocked by

- issues/22-add-jjfx-worker-session-actions.md
