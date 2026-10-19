# Help overlay: move the keybindings off the footer into a `?` menu

Status: done

## Parent

(standalone - no epic)

## What to build

Replace the single dense hint line in the footer with a two-part scheme:

1. **A slim footer** in `Mode::Normal` showing only movement + how to open help,
   e.g. `j/k move  ? help  q quit`. The current all-keys line
   (`app.rs` `footer()`, the `Mode::Normal`/`None` arm) is retired.
2. **A centered modal help overlay**, opened with `?`, listing (almost) every
   keybinding as `label ......... key` rows - action label left-aligned, key
   right-aligned - inside a bordered box drawn over the dimmed workspace list
   (the list stays visible behind it). `?` again or `esc` closes it.

Styling follows the Neovim dashboard reference: calm, one row per binding, key
column right-aligned. No new colours beyond what the TUI already uses.

## Where it plugs in

- **Mode.** Add a `Mode::Help` variant to the `enum Mode` in `app.rs` (peer of
  `Normal`, `NewWorkspace`, `ConfirmDelete`, `ConfirmTidy`). It is a pure UI
  mode - opening/closing it never mutates workspace state.
- **Key dispatch.** In `on_key` add a `Mode::Help => self.on_key_help(key)` arm.
  In `on_key_normal`, bind `KeyCode::Char('?')` to enter `Mode::Help`.
  `on_key_help` handles `?`/`esc` (back to `Normal`) and ignores everything else.
- **Render.** In `render()`, after the list is drawn, if `mode == Help` render
  the overlay on top (a centered `Rect` + `Clear` + a bordered `Block` +
  `Paragraph`/`List` of binding rows). The footer helper keeps its existing
  `NewWorkspace`/`ConfirmDelete`/`ConfirmTidy`/`status` arms unchanged; only the
  `Mode::Normal` + `None`-status arm becomes the slim line.

## Keybindings to list in the overlay

Source of truth is `on_key_normal` today; keep this list in sync with it:

- `j` / `↓` - move down
- `k` / `↑` - move up
- `enter` - open workspace
- `o` - open in background
- `n` - new workspace
- `d` - delete workspace
- `f` - forge selected
- `F` - forge all
- `g` - forge default
- `t` - tidyws (this workspace)
- `T` - tidy (abandon junk empties)
- `c` - fold/expand idle group
- `?` - toggle this help
- `q` / `esc` - quit

Avoid a second hand-maintained copy drifting from the dispatch: prefer a single
`const`/slice of `(label, keys)` that both the overlay and (optionally) a test
can read. The dispatch itself stays a `match` - do not over-engineer a registry.

## Acceptance criteria

- [x] In `Mode::Normal` the footer shows only movement + help + quit, not the
      full key list.
- [x] `?` opens a centered, bordered help overlay listing the bindings above,
      label-left / key-right, over the still-visible (dimmed) list; `?` or `esc`
      closes it and returns to `Normal`.
- [x] Opening/closing help changes no workspace state and emits no status
      message; other modes (new/confirm-delete/confirm-tidy) are unaffected.
- [x] The overlay degrades gracefully in a short/narrow terminal (clamp the
      popup to the frame; never panic or corrupt layout).
- [ ] `mise run check` is green. User-visible, so bump the version (MINOR) and
      add a dated `CHANGELOG.md` entry in the same landing commit.

## Tests

- A `Mode::Help` toggle test in the existing `#[cfg(test)] mod tests` in
  `app.rs`: `press('?')` from `Normal` -> `mode == Help`; `press('?')` /
  `press(Esc)` -> back to `Normal`; assert no `status` is set and selection is
  unchanged.
- If the bindings become a shared slice, a test that its keys are non-empty /
  cover the movement + `?` + `q` rows.

## Notes / decisions (from owner)

- Trigger key: `?`.
- Overlay style: centered modal popup (not full-screen).
- Footer in Normal: move + help + quit.

## Blocked by

- Nothing - `03-skeleton-store` (list render) and the mode/footer machinery are
  already in place on `main`.

## Resolution

Reconciled Mon 19 Oct 2026 against the merged code at `9d6bd0ce6e7b`.

This ticket was marked `done` without its acceptance boxes ticked. Every criterion was re-verified by an independent read-only review of the code and tests on `main`. The reconciliation recorded no new work; it confirms what shipped.

Evidence: `Mode::Help` exists (`src/app.rs:111`) with the `?`/`esc`-only handler at `src/app.rs:971`; the Normal footer is the slim `j/k move ... ? help ... q quit` line at `src/app.rs:2878`. Bindings live in one shared const (`NORMAL_BINDINGS`, `src/app.rs:122`) read by the overlay, not a second hand-maintained list. Tests: `help_toggles_without_touching_state`, `help_overlay_renders_a_bordered_box`, `help_overlay_renders_over_detail_and_graph`, `help_overlay_clamps_to_a_tiny_terminal`, `bindings_cover_the_essential_keys`. The version/changelog half shipped as v0.9.0 (`CHANGELOG.md:636`).

Not ticked: the `mise run check` half of the last criterion. It is a landing-commit property that cannot be re-established by inspection, and this reconciliation deliberately does not run the full suite (the orchestrator runs the gate once at the end).
