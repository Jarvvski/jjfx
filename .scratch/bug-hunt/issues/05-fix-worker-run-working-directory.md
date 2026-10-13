# Fix the Worker Run working directory

Status: ready-for-agent
Type: task
Blocked by: 01

## Problem

A dispatched Worker's agent process runs with the caller's cwd instead of its
Worker Workspace, so Workers share one working copy and their changes are not
separable per ticket. See `@/tmp/jjfx-worker-cwd-bug.md`.

Ticket 01 pins the root cause. The traced code path appears correct
(`src/engine/runtime.rs:990` -> `:654`), so the fix depends on the answer:

- **If it is a jjfx defect** (a spawn path that skips `current_dir`, or a
  `worker_path` that resolves to the wrong root): fix that path and add the
  background-run cwd test permanently.
- **If it is opencode's project-root detection under colocated Git worktrees**:
  work around it at the integration seam - pass an explicit project/working
  directory if opencode supports one, or set the documented env var - and cover
  it with a colocated-worker test.
- **If it is a stale installed binary**: correct the report and keep the test.

## Fix

Implement the fix the ticket-01 answer calls for. Keep the `RunRequest.workspace`
contract ("the Run's working directory") and assert it for every spawn path.

## Commits

1. Keep the background/reserved cwd regression test from ticket 01 (move it into
   `tests/agent_runtime.rs`).
2. Apply the fix.
3. CHANGELOG entry `(fix)`.

## Acceptance Criteria

- [ ] After `jjfx dispatch`, the worker process's cwd equals its Worker
      Workspace.
- [ ] Two Workers dispatched in one call write to two different workspaces.
- [ ] Dispatching from a secondary workspace does not write into it.
- [ ] The regression test passes and is not `#[ignore]`d.
- [ ] `mise run check` is green.

## Out of Scope

- The secondary-workspace root bug (I5, ticket 11), unless ticket 01 shows the
  two share a cause.
