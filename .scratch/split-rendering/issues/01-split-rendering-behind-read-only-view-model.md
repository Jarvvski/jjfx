# Split rendering out of the app.rs god module behind a read-only view model

Status: in-progress

## Parent

(standalone - architecture review 2026-10-08, candidate C5)

## Why

`src/app.rs` (6811 lines) holds model, key handling, all rendering, and four
state machines. Rendering occupies ~1540 lines across two blocks:

- render methods on `impl App` (`app.rs:2113-2956`): `render`,
  `render_send_editor`, `render_pool`, `render_help`, `render_detail`,
  `render_graph_world`, `world_lines`, `header_item`, `workspace_item`,
  `footer`;
- free helpers (`app.rs:2958-3657`): worker line builders, date math, layout
  consts, `pane_border`, colors/glyphs/animations, the graph line builders,
  `centered_rect`.

`render(&mut self)` mutates state during draw: `ensure_selection()`
(`app.rs:2115`), the list cursor via `list.render_body` (`app.rs:2209`), the
world viewport (`app.rs:2214`), the graph viewport (`app.rs:2654`), and,
through `&mut`, the detail viewport/highlighter and task-editor cursor. The one
cross-module leak is real: `diff_view.rs:13` imports `crate::app::pane_border`,
so a renderer depends upward on App.

## What was built

A new `src/render/` module whose external interface is a **read-only view
model**:

- `View<'a>` - read-only projection built by `App::view(&self) -> View<'_>`;
  borrows content only (mode discriminant, classified rows/per-row axes, pool
  session view fields, forge progress, graph ref, status/pending/setup text,
  tick, selected name, idle fold).
- `RenderState` - owns all persistent scroll/cursor state currently split
  across App/Mode: list cursor (`ListState`, out of `WorkspaceList`), world-pane
  viewport (out of `App.world`), graph viewport (out of `Mode::Graph`).
- A prepare/draw split so draw is fully pure:

  ```rust
  pub fn render(&mut self, frame: &mut Frame) {
      self.prepare_layout(frame.area());       // &mut self -> viewport geometry
      let view = self.view();                  // &self -> read-only projection
      render::draw(frame, &view, &self.render); // pure
  }
  ```

Layout:

```
src/render/
  mod.rs      draw(frame, &View<'_>, &RenderState)
  style.rs    pane_border + palette + glyphs/animation + text/time helpers (no App)
  graph.rs    commit_line / world_graph_lines / world_row / workspace_graph_lines / render_graph_pane
  home.rs     list header/row/world-pane rendering from &View
  pool.rs     render_pool
  detail.rs   render_detail
  help.rs     help overlay
  state.rs    RenderState
```

## Commits

1. Add `src/render/style.rs`: move palette (`attention_color`, `agent_color`,
   `brand_color`, `work_color`, `behind_color`, forge colors), glyph/animation
   fns + frame consts, text helpers (`dim_line`, elides), date/time helpers
   (`now_millis`, `parse_rfc3339_millis`, `days_from_civil`, `elapsed_label`,
   freshness), and `pane_border`. Repoint `diff_view.rs` at it; delete app's
   export. This alone removes the only renderer->App leak.
2. Add `src/render/graph.rs`: relocate the already-pure graph functions
   (`commit_line`, `world_graph_lines`, `world_row`, `workspace_graph_lines`,
   `render_graph_pane`). Imports only.
3. Introduce read-only row DTOs reusing workspace_list's types and add
   `src/render/home.rs`: move `header_item`, `workspace_item`, `world_lines`,
   and the home-body layout out of App behind a home view model.
4. Add `src/render/pool.rs`, `src/render/detail.rs`, `src/render/help.rs`: move
   the remaining render bodies behind DTO inputs.
5. Add `src/render/state.rs` (`RenderState`) and make draw pure: move the list
   cursor + world/graph viewports out of App/`Mode::Graph`; add the prepare step;
   keep `App::render(&mut self)` as a thin adapter.
6. Add interface-level render tests (fixture -> TestBackend buffer); rewrite the
   test that reaches past the interface (`app.rs:5406 workspace_item`); assert no
   module under `src/render/` imports `crate::app`.

## Acceptance criteria

- [ ] No module under `src/render/` imports `crate::app`.
- [ ] Rendering lives under `src/render/`; ~1500 lines leave app.rs.
- [ ] `draw` takes only immutable refs; no draw-time state mutation remains.
- [ ] No `.clone()` / `Rc<RefCell<_>>` / `Arc<Mutex<_>>` added to satisfy borrows.
- [ ] All existing render assertions pass unchanged; behavior identical.
- [ ] No version bump / CHANGELOG entry (internal refactor).
- [ ] `mise run check` passes.

## Comments
