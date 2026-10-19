# Workers as child sessions the dispatcher can see

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

A Worker's session is a **top-level** opencode session, so the session that
dispatched it cannot see it, steer it, or collect a structured result - it polls
state files instead. Meanwhile the owner's real workflow *is* nested sessions:
`build` -> `general` -> `reviewer`, 976 `general -> reviewer` edges.

opencode supports this directly: a child session is a session with `parentID`,
and an external process can create one (`POST /api/session`). What is missing is
jjfx's half:

- the opencode plugin emits **no subagent events**;
- the lifecycle fold keys by `cwd` and tracks **at most one session per
  workspace**, so children are invisible as distinct things;
- state is keyed by `WorkerId`, not by run or session.

## Acceptance Criteria

- [ ] A Worker's session is created as a child of the dispatching session when
      one exists, and remains a root session otherwise.
- [ ] The plugin reports child-session lifecycle, and the fold models more than
      one session per workspace without losing the "at most one *agent* per
      workspace" invariant.
- [ ] The parent can enumerate and read its children's state.
- [ ] A test asserts parentage and that a child's completion is visible to the
      parent.

## Out of Scope

- Changing what a Worker *is* - this is topology and observability, not a new
  execution model.

## References

- `assets/opencode/jjfx-lifecycle.ts`, `docs/lifecycle-integration.md`
- `src/agent.rs` (`TrackedAgent`, the fold), `src/engine/opencode_api.rs`
- `docs/adr/0012-named-worker-pools.md`
