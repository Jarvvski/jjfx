# Move blocking work off the TUI thread

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

Several blocking operations run directly inside `App::handle`, stalling input
and redraw:

- **T1** - opening a workspace calls `self.terminal.is_open/open/focus` inline
  (`app.rs:1654-1662`). `KittyTerminal::open` -> `ensure_ready`
  (`terminal.rs:269-299`) runs `kitten @ ls` subprocesses and, when a target is
  configured, sleeps `25 x 200ms = ~5s`. `is_open`/`focus`/`agent_window` also
  shell out.
- **T6** - `t` / `T` call `self.jj.tidyws()` / `tidy()` synchronously
  (`app.rs:1854-1886`), running `jj log` + `jj rebase` / `jj abandon`. Unlike
  lift (`jj.rs:193-195` says "callers must run it off the render loop"), these
  are not backgrounded.

Everything else in the codebase deliberately routes blocking work through
`spawn_blocking`, so this is an inconsistency, not a design choice.

## Fix

Route `open`/`focus`/`close`, `tidy`/`tidyws`, and `Store::reload` behind the
existing `Effect` / `spawn_blocking` seam so `App::handle` is provably
non-blocking. Gate the ticker on "something is animating" so an idle TUI emits
no ticks.

## Commits

1. Move terminal open/focus/close off the UI thread.
2. Move `tidy`/`tidyws` off the UI thread.
3. Move `Store::reload` off the UI thread (or prove it is bounded).
4. Tests: the event loop processes input while each operation is in flight.
5. CHANGELOG entry `(fix)`.

## Acceptance Criteria

- [ ] No subprocess or `sleep` runs on the UI thread.
- [ ] The TUI stays responsive (input + redraw) during open/focus/tidy/reload.
- [ ] `mise run check` is green.

## Out of Scope

- The per-frame work in ticket 26.
