# Cut per-frame and per-tick work in the TUI

Status: resolved
Type: task
Blocked by: none

## Improvement

The render/tick path does more work than the frame needs:

- `lib.rs:405-415` sends `Msg::Tick` every 150ms unconditionally;
  `App::animate` (`app.rs:535-550`) calls `agent_state` per workspace until it
  finds a `Working` one, and `agent_state` -> `AgentStates::agent_for`
  canonicalises the path on every call (`agent.rs:158-160`, `:226-231`). On an
  idle screen that is N `canonicalize` syscalls every 150ms on the UI thread.
- `App::prepare_layout` (`app.rs:2206-2243`) calls `render::graph::world_lines`
  purely for its `.len()`, then `render::home::draw` (`render/home.rs:68-69`)
  and `render::graph::draw_world` (`render/graph.rs:342`) call it again - the
  full renderdag layout runs twice per frame. `row_inputs()` also runs twice
  (`app.rs:2089-2092` then `:2247-2248`), each canonicalising every workspace.

## Fix

- Cache agent state per workspace (canonicalise once, invalidate on path
  change).
- Compute graph lines once per frame and pass them from `prepare_layout` into
  the renderers (or have renderers return the height).
- Gate the ticker on "something is animating" so an idle TUI emits no ticks.

## Commits

1. Agent-state cache.
2. Single graph-line computation per frame.
3. Idle ticker gating.
4. CHANGELOG entry `(add)` (internal; skip if no user-visible change).

## Acceptance Criteria

- [x] No per-tick filesystem canonicalisation on an idle screen.
- [x] `world_lines` runs once per frame.
- [x] Rendering output is unchanged.
- [x] `mise run check` is green.

## Outcome (2026-10-13)

All three sub-items landed.

1. **Agent-state cache.** `AgentStates` gains a `keys: HashMap<PathBuf, PathBuf>`
   memo (workspace path -> canonical join key) and a `track()` that resolves it.
   `agent_for` is now a hash probe, falling back to `canonicalize` only for an
   untracked path. `App::track_agent_paths` re-resolves on `App::new` and
   `after_store_changed`, i.e. exactly when the workspace set changes.
2. **One graph layout per frame.** `prepare_layout` returns the laid-out
   `Vec<Line>` and `App::render` hands it to whichever renderer draws the pane;
   `HomeView.graph` and the second `world_lines` call in `home::draw` are gone.
   `home::draw` still decides *whether* to show the pane from the real body
   height, so the graceful-degradation rule is unchanged (and now has a test
   that supplies lines for a too-short pane).
3. **Idle ticker gating.** `spawn_animation_ticker` takes a shared
   `Arc<AtomicBool>` that the event loop republishes after every batch, so an
   idle TUI emits no `Msg::Tick` and the loop stays parked on its message
   channel instead of waking ~7 times a second. `animate` was split into a pure
   `is_animating()` (the gate) and `animate()` (the frame advance).

**Left undone, deliberately:** `row_inputs()` still runs twice per frame
(`ensure_selection` and `home_view` each project the workspace set). Removing it
means restructuring borrows - `RowInput<'_>` borrows `self.store`, which blocks
the `&mut self` viewport pass - and that was judged riskier than its value. Its
expensive part (the per-workspace `canonicalize`) is now cached, so the residual
cost is 2N hash probes.

**Tests (4):** `agent::a_tracked_path_reuses_its_canonical_key` (a retargeted
symlink proves resolve-once, and re-`track` picks up the move),
`agent::tracking_a_path_leaves_untracked_lookups_correct`,
`app::is_animating_reports_the_gate_without_advancing_the_frame`, and
`lib::animation_ticker_is_silent_until_something_animates` (600ms margins around
a 150ms interval, so scheduling jitter cannot false-fail).

**Residual risk:** the memo is only re-resolved on a store change, so a workspace
directory appearing without a reload would keep its fallback key - not reachable
via the store, which reports a path only for an existing directory. `world_lines`
now samples `now_millis()` once per frame instead of twice; freshness styling is
coarse, so render output is unchanged (the existing render tests pass).

**Environmental note:** the first full-gate run hit a pre-existing flake
(`tests/agent_runtime.rs::reset_run_cannot_clear_a_newer_run_reserved_during_process_cleanup`
read an empty process-identity file) under very high machine load from three
concurrent workspace builds. It passed in isolation and on 3 repeat runs, and
the final full gate was green. No files in that area were touched.

## Out of Scope

- The blocking-work move (ticket 17).
