# Configurable window layouts per display

Status: resolved

## Parent

(standalone)

## Why

Opening a workspace (or mounting a Worker) builds one fixed layout: a 19% left
column with the first pane over a second shell, and the agent filling the
remaining 81%. On the laptop display that column is too narrow to read the dev
pane comfortably, while on the external monitor the current proportions are
right. The layout should follow the display, and the arrangement itself should
be configurable rather than hardcoded in two places (`src/terminal.rs` and
`crates/wsg-core/src/worker_actions.rs`).

## What to build

- A declarative pane plan: an ordered pane list with `role` (`first` | `agent`
  | `shell`), `from` (anchor pane index, default 0), `location` (`vsplit` |
  `hsplit`, required after the first pane), and `bias` (percentage given to the
  new pane, default 50, valid 1..=99).
- Named layouts under `[terminal.layouts.<name>]`, selected per display
  condition by `[terminal.layout_by_display]` with `laptop`, `external`, and
  `default` keys. Zero config keeps today's exact layout everywhere.
- Display detection: external display attached?
  (`system_profiler SPDisplaysDataType -json`; external means a connection type
  that is not `spdisplays_internal` and a display type that is not
  `spdisplays_built-in*`). Detection runs per open so plugging a monitor in
  mid-session is picked up. Undetectable (non-macOS, command failure) falls back
  to `default`, then to the built-in layout.
- `JJFX_LAYOUT=<name>` forces a named layout; an unknown name fails the open
  with a clear message.
- Both execution paths honor the plan: workspace tabs (`KittyTerminal::open`)
  and worker Mount (`SystemCommands::mount`). Role-to-command policy stays local
  to each driver.
- Config validation at load: every mapping target exists, profiles have at least
  one pane, pane 0 has no `location`, later panes have one, `from` points at an
  earlier pane, bias is in range. A bad config fails before the TUI opens.

## Config shape

```toml
[terminal]
first_pane_command = "mise app:dev"

[terminal.layout_by_display]
laptop = "narrow"
external = "wide"
default = "wide"

[terminal.layouts.wide]
[[terminal.layouts.wide.panes]]
role = "first"
[[terminal.layouts.wide.panes]]
role = "agent"
location = "vsplit"
bias = 81
[[terminal.layouts.wide.panes]]
role = "shell"
location = "hsplit"
bias = 50

[terminal.layouts.narrow]
[[terminal.layouts.narrow.panes]]
role = "first"
[[terminal.layouts.narrow.panes]]
role = "agent"
location = "vsplit"
bias = 70
[[terminal.layouts.narrow.panes]]
role = "shell"
location = "hsplit"
bias = 50
```

## Commits

1. wsg-core: pane-layout plan model, validation, and plan-driven Mount whose
   default reproduces today's argv.
2. jjfx: layout config surface, display probe, `KittyTerminal` builds tabs from
   the resolved layout.
3. jjfx: worker `mount` uses the shared settings seam; version bump and
   changelog entry.

## Acceptance criteria

- [x] No config: identical argv to today for both workspace tabs and Mount.
- [x] Laptop-only resolves the `laptop` layout; external attached resolves the
      `external` layout; both verified with unit tests over fixtures.
- [x] `JJFX_LAYOUT` overrides the condition; unknown names error.
- [x] Invalid layouts and dangling mapping targets are startup errors.
- [x] Focus still lands on the agent (else the first pane) and background opens
      still never raise the target.
- [x] `mise run check` passes.

## Comments

- 2026-09-18: filed from the planning session; implementation starts on the
  stack above `main`.
- 2026-09-18: landed as three focused commits above `main`:
  - `Build mounted worker tabs from a pane layout plan`: `wsg-core` gains the
    `PaneLayout`/`PanePlacement`/`PaneRole`/`SplitLocation` model with
    validation, and `SystemCommands::mount` walks the plan instead of hardcoding
    splits. The built-in plan reproduces the previous argv.
  - `Choose a workspace tab layout from the display and config`: `terminal.layouts`
    and `terminal.layout_by_display` parse and validate at load; `src/display.rs`
    classifies the `system_profiler` report; `LayoutSettings` selects the layout
    (forced profile, condition mapping, `default`, built-in); `KittyTerminal`
    builds tabs from `open_steps`.
  - `Mount workers with the configured display layout`: the CLI seam now passes
    a `TerminalSettings` (first-pane command + display-selected layout) into
    `WorkerActions`, so `mount` honors the same profiles; version bump and
    changelog entry.
- 2026-09-18: the external-display fixture is a plausible report, not yet a
  captured one. Confirm with a real `system_profiler SPDisplaysDataType -json`
  dump while the monitor is attached and adjust the classifier if the keys
  differ.
- 2026-09-18: follow-up landed above `main` as `Focus the agent pane when
  entering an open workspace`: panes are tagged at launch with
  `--var jjfx_role=<role>` and `KittyTerminal::focus` looks the agent pane up in
  the reopened tab, falling back to the configured agent command for tabs
  opened before the tag. Background opens (`--dont-take-focus`) cannot set the
  active pane without raising the OS window, so their agent takes focus once the
  tab is entered through jjfx (jjfx 0.48.1).
