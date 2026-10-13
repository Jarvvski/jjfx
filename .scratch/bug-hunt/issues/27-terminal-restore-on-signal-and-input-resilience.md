# Restore the terminal on signal and keep input alive

Status: resolved
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

- [x] SIGTERM/SIGINT restores the terminal.
- [x] A panic during terminal setup restores the terminal.
- [x] An input-read error does not silently kill input.
- [x] `mise run check` is green.

## Outcome (2026-10-13)

### T8 - signal restore

`Session::enter` now installs the panic hook **and** the signal handler before it
touches the terminal, so a panic or a fatal signal anywhere in setup is covered.
`restore()` is already idempotent and a no-op before any mode is enabled
(crossterm's `disable_raw_mode` returns early when raw mode was never enabled),
which is what makes installing first safe - now pinned by a test.

The handler is deliberately **not** a raw `sigaction` callback. `unsafe` is
forbidden crate-wide (`Cargo.toml` `[lints.rust] unsafe_code = "forbid"`) and
`restore()` is not async-signal-safe (it locks, formats escape sequences, and
allocates). `signal-hook` delivers the signal to a dedicated thread instead, so
restoration runs in ordinary thread context. `signal-hook` is **not a new build
dependency**: crossterm already pulls it in (twice - for ratatui and
ratatui-textarea), so adding it to `[dependencies]` changes no crate in the build
graph.

After restoring, the thread re-raises under the default disposition
(`emulate_default_handler`) so a supervisor sees the process die *from* the
signal rather than a plain exit that hides what happened, with a
`std::process::exit(128 + signal)` fallback.

### T9 - resilient input reader

`classify_read` splits the one `event::read()` outcome three ways:
`Interrupted` is **retried** (a signal arriving mid-read leaves the event
pending, so retrying loses nothing - the old code treated it as fatal and killed
input for the rest of the session), anything else becomes `Fatal` and is sent as
`Msg::InputFailed`. The app **pins** a footer message for it rather than using an
expiring status, because the condition cannot clear itself and a message that
timed out would leave a UI that silently ignores keys.

The reader thread is still never joined, deliberately: it is parked in a
blocking `event::read()`, so joining would hang until the next keypress. It
exits on its own when the receiver drops, or dies with the process, and holds no
state worth flushing. Making it joinable would mean polling instead of blocking,
which would reintroduce the idle wakeups ticket 26 just removed.

## Verification

**Pre-fix failure captured.** With `install_signal_restore()?` removed, the new
PTY test fails exactly as the ticket describes - the helper enters the alternate
screen and never leaves it:

```
SIGTERM should leave the alternate screen:
[?1049h[?2004h[>1u        <- entered, killed, never restored
```

**New PTY tests (3).** `sigterm_/sigint_/sighup_in_the_shared_tui_restores_the_terminal`
spawn a helper that enters the real TUI and holds it, wait until the
leave-...-enter sequence `\u{1b}[?1049h` appears on the PTY, send the signal,
then assert the PTY saw `\u{1b}[?1049l` and that the child died from the signal.
One case per signal pins the registered *list*, not just the mechanism: if the
handler only registered SIGTERM, the SIGINT and SIGHUP cases would fail.

**New unit tests (5).** `tui::restore_is_a_no_op_before_any_mode_was_enabled`,
`tui::a_panic_restores_the_terminal` (observes the hook through a test-only
restore counter rather than setting a mode flag, which would make `restore` write
escape sequences into the test runner's stdout),
`tui::installing_signal_restore_is_idempotent`,
`lib::an_interrupted_read_is_retried_and_a_fatal_one_is_surfaced`, and
`app::a_dead_input_reader_pins_a_status_instead_of_failing_silently`.

The three PTY tests are `#[ignore]`d and run under `mise run test-deep`, matching
the existing `panic_in_the_shared_tui_restores_the_terminal`.

## Residual limitation

T9's *policy* is unit-tested but its wiring is not: `spawn_input_reader` reads
the real stdin through `event::read()`, so driving it into the `Fatal` arm in a
test would mean either stealing the test runner's stdin or relying on stdin not
being a TTY - both environment-dependent. The wiring is the one-line match in
`spawn_input_reader`; reaching it needs a fake event source.

## Out of Scope

- The help-overlay issues (ticket 18, 28).
