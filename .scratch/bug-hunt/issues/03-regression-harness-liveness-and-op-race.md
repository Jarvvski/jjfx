# Regression tests: failed-dependency liveness and the Reset/`send` race

Status: resolved
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

- [x] Both tests exist and fail against `main` without hanging.
- [x] The liveness test fails by *observation* (non-terminal after N cycles),
      not by timeout.

## Outcome (2026-10-13)

**Resolved, tests-only** (`jj diff -- src` is empty). One commit,
`Guard failed-blocker liveness and Rebase serialisation with regression tests`.

Both fixes had already landed, so the tests are green on the current tree - the
ticket's "builds the red tests" framing predates them. Each test was therefore
proved real by **temporarily reverting the fix it guards** and capturing the red
output:

- `tests/orchestration.rs::advance_once_observes_a_failed_blocker_group_as_terminal`
  persists a group whose blocker is `Failed` with a `pending` dependent, then
  calls the runner's `advance_once` in a bounded 8-cycle loop. It fails by
  observation (still non-terminal after the budget), not by `max_cycles` or a
  hang. Reverting `DispatchGroup::is_terminal` to `values().all(is_terminal)`
  makes it fail in 0.06s with
  `a group whose only remaining work is unreachable must be observed as terminal`.
  This complements the existing `run`-based test by pinning the runner's own
  advance step.
- `tests/worker_actions.rs::rebase_waits_for_the_workspace_operation_lock` holds
  the `.jj/pool/<worker>.workspace.lock` sidecar in-process, spawns the
  `repository_action_helper` subprocess running `rebase`, asserts it is still
  blocked after 400ms, then asserts success and the exact
  `jj rebase` / `jj git push` / `gh pr view` order. Removing
  `lock_worker_operation` from `follow_up` and `rebase` fails both this test and
  the existing `send_waits_for_the_workspace_operation_lock`, so ticket 08's fix
  is now guarded on both halves - previously only `follow_up` was.

**Deliberately not added:** a reset-then-`send` ordering test. The reset
restoration thread resolves `jj` through the shared process PATH, so it cannot be
slowed deterministically without racing parallel tests; `follow_up`'s lock (which
is what that scenario reduces to) is already asserted, and reproven red above.

**Residual risk:** the rebase test has a 400ms blocked-window, mirroring the
existing send test's convention. Under heavy concurrent load it may need a re-run
in isolation.

## Out of Scope

- The fixes (tickets 04, 08).
