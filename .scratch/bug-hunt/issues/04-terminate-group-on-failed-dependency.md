# Terminate a Dispatch Group when a dependency fails

Status: resolved
Type: task
Blocked by: none

## Problem

A terminally `Failed` Sub-issue permanently blocked every Sub-issue that listed
it as a Blocker, so the group never reached a terminal state and
`OrchestrationRunner::run` polled forever at 5s while holding the orchestrate
flock. There is no production `max_cycles`, so the thread and lock leaked until
the process was killed.

- `SubIssueStatus::unblocks()` = `Done | Skipped`
  (`src/engine/dispatch_group.rs`).
- `is_terminal()` was `all(terminal)`, so a `pending` dependent kept it false.
- `run` (`src/engine/orchestration.rs`) looped with no liveness exit.

## Fix

`DispatchGroup::is_terminal()` now means **quiescent**: no Sub-issue is
`Dispatched` and `ready()` is empty. In a validated DAG (no cycles) that is
equivalent to "no further progress is possible", so a group whose remaining work
is unreachable by a failed dependency terminates with a summary.

A new `blocked` count (fixed-point reachability: `Done`/`Skipped` satisfy,
`Dispatched` is in flight, `Failed` never satisfies) is reported in
`DispatchGroupStatusCounts`, the progress projection, the Pool view, and the
`Orchestration complete:` line, so the unreachable Sub-issues are visible.

## Decision Document

**No default `max_cycles`.** A group that is `ready()` but has no free Worker is
legitimately waiting for capacity, and a cycle cap would eventually fail it.
Quiescence is the correct terminal condition; `max_cycles` stays an opt-in bound
used by tests. A genuinely stuck group is now quiescent and terminates, so the
cap is no longer load-bearing for liveness.

## Commits

1. `Terminate a Dispatch Group when a dependency fails` - the quiescence
   predicate, the `blocked` count, UI/CLI surfacing, and tests, with a CHANGELOG
   `(fix)`.

## Acceptance Criteria

- [x] A group containing a failed blocker reaches a terminal state.
- [x] `run` cannot poll such a group forever (covered by the bounded-budget
      orchestration test, which failed before the fix and passes after).
- [x] The unreachable Sub-issues are visible (`blocked` count in the summary,
      Pool view, and CLI).
- [x] `mise run check` is green.

## Out of Scope

- Auto-retrying a failed Sub-issue.
- Changing the frozen wire vocabulary (no new status value was added).
