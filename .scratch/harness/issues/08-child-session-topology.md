# Workers as child sessions the dispatcher can see

Status: resolved
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

- [x] A Worker's session is created as a child of the dispatching session when
      one exists, and remains a root session otherwise.
- [x] The plugin reports child-session lifecycle, and the fold models more than
      one session per workspace without losing the "at most one *agent* per
      workspace" invariant.
- [x] The parent can enumerate and read its children's state.
- [x] A test asserts parentage and that a child's completion is visible to the
      parent.

## Out of Scope

- Changing what a Worker *is* - this is topology and observability, not a new
  execution model.

## References

- `assets/opencode/jjfx-lifecycle.ts`, `docs/lifecycle-integration.md`
- `src/agent.rs` (`TrackedAgent`, the fold), `src/engine/opencode_api.rs`
- `docs/adr/0012-named-worker-pools.md`

## Resolution

A Worker's session is now created as a child of the session that dispatched it,
and stays a root session when nothing did. `OpenCodeApi` gained `parentID` on
`session.create` (`NewSession::with_parent`) and the dispatching session is read
from the `OPENCODE_SESSION_ID` opencode injects into a session's shell commands
(`opencode_api::dispatching_session`), so a Dispatch issued from an agent's
shell nests the Worker and a Dispatch from the TUI, a terminal or CI does not.
`RunSupervisor::prepare_reserved_session` threads it into `create_run_session`,
so both the session-delivered and the process-fallback Run get it.

The opencode plugin remembers each session's parent when it is created and
stamps `parent_id` on that session's whole lifecycle, so every line is
self-describing rather than only the start. It also maps the v2 execution
events (`session.execution.started` / `succeeded` / `failed` / `interrupted`) to
the same working/stop/interrupt states as the older `session.status`/`session.idle`
pair, which is what a current opencode actually emits for a turn.

The fold keeps "at most one agent per workspace": a session that names a parent
is folded on its own axis in `AgentStates` (keyed by session id), so a child can
never displace the agent that owns the workspace, while the parent reads it back
through `AgentStates::children_of`.

**Criterion 3 - "the parent can enumerate and read its children's state" - is
satisfied by a real surface: `jjfx children [<session>] [--json]`.** The shape is
a subcommand, not a `status --json` field, because children are session-scoped
and `status` is repository-scoped: a parent must be able to ask from anywhere,
including a shell outside the repository it dispatched into, so `children` runs
before repo discovery like `hooks`. A parent reaches it with no argument at all
- opencode puts the session's id in `OPENCODE_SESSION_ID`, so a bare
`jjfx children` lists the sessions *that* session dispatched - and naming a
session lists another one's. Each row carries the child's folded state, plus the
title and directory opencode supplies (which Worker it is). The children are the
union of what opencode reports (`OpenCodeApi::child_sessions`, filtering
`session.list` on the server and again client-side) and what the lifecycle log
has seen, so a child opencode knows but the log has not is listed as absent
rather than guessed, and the command still names every child and its state when
opencode is unreachable (the `source` field says which happened).

Verified: `mise run check` (changelog validation + fmt + lint + build + test)
passes. The surface is tested by driving the built binary, not just the library:
`tests/children.rs` runs `jjfx children` against a lifecycle log and asserts a
parent's two children are listed with `working` and `waiting`, that a bare
invocation defaults to `OPENCODE_SESSION_ID`, that a session with no children
reports none, that a missing parent names the requirement, that `--json` carries
every child and its state, and that with a fake `opencode` on `PATH` the
authority is opencode's list and its titles (a log-unknown child shows as
`absent`). `opencode_run_creates_a_child_session_of_the_session_that_dispatched_it`
drives a real dispatch with a fake `opencode` and asserts the `session.create`
body carries `parentID` (and the anchor is still the Worker Workspace);
`opencode_run_prompts_its_session_with_no_client_process` asserts a root
session. `a_childs_completion_is_visible_to_its_parent` folds a parent and child
through the real `parse_line` seam and asserts the parent sees the child settle
and end while its own state is untouched. The TypeScript was verified by running
the real `assets/opencode/jjfx-lifecycle.ts` under `bun` against synthetic
events and asserting the JSONL it writes - parentage on a child's start, prompt,
stop, interrupt, permission and end, none on a root, and parentage dropped once
the session is deleted.
