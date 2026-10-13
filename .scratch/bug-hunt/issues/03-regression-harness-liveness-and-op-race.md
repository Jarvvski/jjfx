# Regression tests: failed-dependency liveness and the Reset/`send` race

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

Two high-severity defects are unguarded by tests. This ticket only builds the
red tests; the fixes are tickets 04 and 08.

### D1 - a failed dependency never releases the group

`SubIssueStatus::unblocks()` is `Done | Skipped`
(`src/engine/dispatch_group.rs:55-57`), and `is_terminal()` is `!is_active()`
(`:50`). A `Failed` blocker therefore leaves its dependents `pending` forever,
so the group is never terminal and `OrchestrationRunner::run`
(`src/engine/orchestration.rs:1064-1077`) polls at 5s holding the orchestrate
flock, with no production `max_cycles`. The existing test
`tests/dispatch_group.rs:300-324` encodes "failed blockers as blocking" but
never reaches a terminal group, so the liveness gap is unguarded.

### C1 - the workspace-op lock is not taken by `send`/`review`/`rebase`

`worker_actions.rs:579-643` (`follow_up`) and `:370-387` (`rebase`) never call
`lock_worker_operation`, while `reset` (`:453-481`) takes it and then hands the
guard to a detached restoration thread that runs `jj restore` + `jj new main`.
The Worker is marked idle by `reset_run` before restoration finishes, so a
follow-up Run can edit the working copy mid-restore.

## Fix

Add integration tests (no production change in this ticket):

1. A group whose blocking Sub-issue is `Failed`: assert `advance_once` reaches a
   terminal state (currently returns `None` forever). Use a bounded loop so the
   test fails rather than hangs.
2. Reset a Worker and immediately `send` a follow-up: assert the workspace
   operations are serialised (a follow-up does not observe a half-restored
   working copy).

## Commits

1. Add the failed-blocker liveness test (bounded, expected to fail).
2. Add the Reset/`send` serialisation test (expected to fail).

## Acceptance Criteria

- [ ] Both tests exist and fail against `main` without hanging.
- [ ] The liveness test fails by *observation* (non-terminal after N cycles),
      not by timeout.

## Out of Scope

- The fixes (tickets 04, 08).
