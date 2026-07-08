# Walking skeleton: workspace store, cache mirror, list render

Status: ready-for-agent

## Parent

epics/A-foundations.md

## What to build

The end-to-end skeleton the whole app hangs off: a ratatui + crossterm + tokio
application that loads the authoritative per-repo workspace store, reconciles it
with the existing `.jj/ws-cache` (reading it so shell-created workspaces appear,
writing through so the bash tools stay consistent - ADR 0006), watches the cache
file for live changes, renders a plain workspace list, and quits cleanly with a
restore-on-panic guard so a crash never corrupts the terminal. No lifecycle
state yet - just the workspaces and their paths.

## Acceptance criteria

- [ ] Launching jjfx in a jj repo lists its workspaces (default + named), byte-compatible with `.jj/ws-cache` (`name\tpath`).
- [ ] A workspace created in a shell (`jj ws add x`) appears in the running TUI without a restart.
- [ ] The authoritative store round-trips: jjfx-side changes write through to `.jj/ws-cache` atomically (temp-file + rename), and the bash tools read them.
- [ ] `q`/esc quits; a panic restores the terminal via the panic hook; `mise run check` passes.

## Blocked by

None - can start immediately.
