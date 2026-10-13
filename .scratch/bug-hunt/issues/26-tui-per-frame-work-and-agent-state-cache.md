# Cut per-frame and per-tick work in the TUI

Status: ready-for-agent
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

- [ ] No per-tick filesystem canonicalisation on an idle screen.
- [ ] `world_lines` runs once per frame.
- [ ] Rendering output is unchanged.
- [ ] `mise run check` is green.

## Out of Scope

- The blocking-work move (ticket 17).
