# Draw the help overlay in Detail and Graph

Status: resolved
Type: task
Blocked by: none

## Problem

Help can be opened from any mode (`app.rs`), but `App::render` early-returned
for the base mode before drawing the overlay in Detail and Graph. Meanwhile
`on_key_help` swallowed every key except `?`/`Esc`. So pressing `?` over Detail
or Graph showed nothing and froze `j/k` and `esc`-to-close until `?`/`Esc` was
pressed again. Tests only covered Help over Normal/Pool.

## Fix

Draw the help overlay **after** the base view in Detail and Graph, with new
`DETAIL_BINDINGS` and `GRAPH_BINDINGS` that describe those modes' keys rather
than the list keys.

## Commits

1. `Draw the help overlay in Detail and Graph` - the two binding sets, the
   render change, a render test covering both base modes, and a CHANGELOG
   `(fix)`.

## Acceptance Criteria

- [x] `?` over Detail and Graph shows the help overlay.
- [x] A render test covers `Help(Detail)` and `Help(Graph)`.
- [x] `mise run check` is green.

## Out of Scope

- The help-over-editor and `?`-in-text-buffer issues (ticket 28).
