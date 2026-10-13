# Restore the terminal on signal and keep input alive

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

- **T8** - `tui.rs:94-98` (Drop) and `:130-139` (panic hook) restore the
  terminal, but an external `kill`/SIGTERM or a closed terminal window bypasses
  both, leaving the shell in raw mode / the alternate screen. The panic hook is
  also installed only after the alt screen is entered (`:66`), so a panic in the
  `execute!` calls between `enable_raw_mode` and `install_panic_hook` is not
  covered.
- **T9** - the input reader thread exits on the first `event::read()` error
  (`lib.rs:419-427`) with no log or recovery, permanently making the TUI
  input-dead while still rendering. The thread is never joined.

## Fix

- Install a SIGINT/SIGTERM/SIGHUP handler that calls `restore()` (safe,
  no `unsafe` - use a signal-safe mechanism).
- Move panic-hook installation before the alt screen is entered.
- Make the input reader resilient: log/recover from transient errors and
  surface a fatal one to the app instead of silently dying.

## Commits

1. Signal handler + restore; reorder hook installation.
2. Resilient input reader with a surfaced fatal path.
3. Tests where feasible (panic restore; reader error handling).
4. CHANGELOG entry `(fix)`.

## Acceptance Criteria

- [ ] SIGTERM/SIGINT restores the terminal.
- [ ] A panic during terminal setup restores the terminal.
- [ ] An input-read error does not silently kill input.
- [ ] `mise run check` is green.

## Out of Scope

- The help-overlay issues (ticket 18, 28).
