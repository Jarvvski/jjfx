# Restore wsg Workspace and Worker Pool CLI compatibility

Status: ready-for-agent

## Parent

epics/D-interfaces-and-cutover.md

## Problem Statement

The Rust shared library is not a replacement for existing scripts until a binary preserves wsg's Workspace and Worker Pool commands, aliases, output channels, confirmation behavior, and machine-readable paths. Reimplementing rules inside command handlers would undermine the shared library.

## Solution

Build thin command adapters over the shared Repository and Worker Pool interfaces for Workspace management, pool lifecycle, status, version, and default behavior prerequisites. Match the command contract inventory while keeping formatted output and interactive confirmation in the CLI package.

## Commits

1. Add command parsing and help for Workspace and Worker Pool command groups.
2. Restore Repository root, where, path, and refresh commands with machine-readable stdout.
3. Restore Workspace add, remove, list, and clean with existing aliases and confirmations.
4. Restore Worker Pool create, resize, list, named remove, Reset, and destroy.
5. Restore status as the pool-list alias and preserve aligned human output.
6. Restore Worker alias display and input resolution.
7. Restore version output with an explicit wsg package version independent of jjfx.
8. Add shell-level contract tests for stdout, stderr, exit status, aliases, and non-TTY behavior.
9. Update build and install tasks to produce the Rust wsg binary without replacing the installed Go binary yet.

## Decision Document

- CLI handlers contain no lifecycle implementation.
- Machine-readable values go to stdout; progress and diagnostics go to stderr.
- Confirmation stays in the frontend and passes an explicit decision into the library.
- The wsg package maintains its own semver even though it shares one repository with jjfx.
- No-argument TUI behavior waits for the jjfx integration ticket.

## Testing Decisions

Use black-box binary tests against temporary jj repositories and compare semantic output with the command contract inventory. Cover missing arguments, unknown Workers, non-TTY confirmation, aliases, malformed state, and command failures. Do not pin ANSI bytes unless required by scripts.

## Acceptance Criteria

- [ ] Workspace and Worker Pool commands cover the current compatibility inventory.
- [ ] Scripts receive paths only on stdout.
- [ ] Status reconciles dead Workers before display.
- [ ] Versioning is explicit for both binaries.
- [ ] Installing for test does not overwrite the user's current Go wsg binary.
- [ ] `mise run check` is green.

## Out of Scope

- Dispatch and Follow-up commands
- Shell completion
- jjfx TUI controls
- Final installation cutover

## Blocked by

- issues/07-port-workspace-and-pool-lifecycle.md
- issues/09-port-logs-sessions-and-worker-actions.md
