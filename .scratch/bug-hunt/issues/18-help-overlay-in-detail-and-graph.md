# Draw the help overlay in Detail and Graph

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

Help can be opened from any mode (`app.rs:680-683`), but `App::render`
early-returns for the base mode before drawing the overlay:

- Detail: `app.rs:2173-2176`.
- Graph: `app.rs:2177-2185`.

Meanwhile `on_key_help` (`app.rs:814-821`) swallows every key except `?`/`Esc`.
So pressing `?` over Detail or Graph shows nothing and freezes `j/k` and
`esc`-to-close until `?`/`Esc` is pressed again. Tests only cover Help over
Normal/Pool (`app.rs:2876-2907`).

## Fix

Draw the help overlay **after** the base view in Detail and Graph too (or
disallow opening help there). Add render tests for `Help(Detail)` and
`Help(Graph)`.

## Commits

1. Draw the overlay after the base view in both modes.
2. Add the two render tests.
3. CHANGELOG entry `(fix)`.

## Acceptance Criteria

- [ ] `?` over Detail and Graph shows the help overlay.
- [ ] `j/k` and `esc` behave correctly while help is open in both modes.
- [ ] `mise run check` is green.

## Out of Scope

- The help-over-editor and `?`-in-buffer issues (ticket 28).
