# Move blocking work off the TUI thread

Status: ready-for-agent
Type: task
Blocked by: none

## Progress (2026-10-13)

**T1 and T6 are fixed.** T1's terminal `is_open`/`open`/`focus` and T6's
`tidy`/`tidyws` now run through the `Effect` / `spawn_blocking` seam:

- New effects `OpenWorkspace { name, path, focus, created }` and
  `Tidy { kind }`, reported as `Msg::WorkspaceOpened` / `Msg::Tidied`.
- The `Jj` seam moved from `App` to `Runtime` (the owner of blocking work), so
  the App no longer holds a `Jj` at all and `App::new` lost that parameter.
  `Jj` is now `Send + Sync` so an `Arc<dyn Jj>` can cross into a blocking worker.
- The App's terminal field went too: the only remaining production user was the
  dispatch controller, built in `App::new`.

**`Store::reload` is split into ticket 30.** It is the third blocking path in
this ticket (`Store::load` -> `jj workspace list`), but backgrounding it changes
*when* the UI reflects disk state - it becomes eventually consistent, and
`Msg::Reload`/`App::reload` call sites and their tests all shift. That deserves
its own change and tests rather than being folded in here.

## Problem

Several blocking operations ran directly inside `App::handle`, stalling input and
redraw:

- **T1** - opening a workspace called `self.terminal.is_open/open/focus` inline.
  `KittyTerminal::open` -> `ensure_ready` runs `kitten @ ls` subprocesses and,
  when a target is configured, sleeps `25 x 200ms = ~5s`.
- **T6** - `t` / `T` called `self.jj.tidyws()` / `tidy()` synchronously, running
  `jj log` + `jj rebase` / `jj abandon` on the UI thread. Unlike lift, these were
  not backgrounded.

Everything else in the codebase deliberately routes blocking work through
`spawn_blocking`, so this was an inconsistency, not a design choice.

## Commits

1. `Run tab opens and tidy sweeps off the input fold` - both paths, the moved
   `Jj` seam, the test-harness changes, and a CHANGELOG `(fix)`.

## Testing Decisions

- `a_slow_tab_open_does_not_block_input` - the fake terminal's `open` sleeps
  500ms; a subsequent key is folded and the row moves while `open` is still
  running, then the open completes. This is the "input is processed while the
  operation is in flight" acceptance criterion.
- `tidyws_requests_the_sweep_as_an_effect` / `tidy_runs_only_after_confirmation_...`
  - the fold requests the effect (the `Lift` pattern) rather than calling jj.
- The status-message coverage moved to `#[tokio::test]`s that drive the effect
  through the `Runtime` and await `Msg::Tidied`, so the wording is still asserted.

## Acceptance Criteria

- [x] No subprocess or `sleep` runs on the UI thread for T1 and T6.
- [x] The TUI stays responsive during open/focus/tidy.
- [ ] `Store::reload` still runs on the UI thread (ticket 30).
- [x] `mise run check` is green.

## Out of Scope

- The per-frame work in ticket 26.
- `Store::reload` (ticket 30).
