# Non-todo dependency statuses satisfy their dependents

Status: ready-for-human
Type: bug
Blocked by: none

## Question

Should a dependency that is merely `In Progress` or `In Review` - not completed -
satisfy its dependents?

Split out of ticket 28 (item D8). The code contradicts the glossary, so this
looks like a real defect; but "block until done" reintroduces the liveness
tension ticket 04 wrestled with, so it needs an intent decision.

## What the code does today

`DispatchGroup::from_dependency_graph` (`src/engine/dispatch_group.rs:355-361`)
marks any Sub-issue whose Linear status is **not** `backlog`/`todo`/`triage`
(`is_dispatchable_status`, `:856-861`) as `status = "skipped"`, recording the raw
status as `skip_reason`. `In Progress` and `In Review` land here.

`unblocks()` (`:55-57`) returns `matches!(self, Done | Skipped)`, and `ready()`
(`:469-484`) treats a Pending ticket as ready once every blocker `unblocks()`.
So an `In Progress` blocker satisfies the dependency.

**Minimal scenario.** `from_dependency_graph` over
`[{"id":"ENG-101","status":"In Progress","blocked_by":[]},
  {"id":"ENG-102","status":"Todo","blocked_by":["ENG-101"]}]`
gives ENG-101 `status = "skipped", skip_reason = Some("In Progress")`, and
`ready()` then returns `[ENG-102]` - ENG-102 is dispatched while its dependency
is still in progress.

## Docs it contradicts

- `CONTEXT.md:168-171` - "**Dependency**: A Sub-issue that **must be completed**
  before another Sub-issue becomes eligible for Dispatch." (`_Avoid_: blocker`)
- `CONTEXT.md:192-195` - "**Dispatch Wave**: The set of Sub-issues … whose
  Dependencies are **satisfied** and that may be dispatched concurrently."
- `.scratch/wsg-migration/issues/12-port-dispatch-group-model.md:44` -
  "**Merged** Sub-issues satisfy downstream Dependencies." (names only
  Merged/Done, not in-flight statuses)

Against the code's own intent: the `Skipped` doc
(`src/engine/dispatch_group.rs:28-29`) scopes skipping to "already delivered or
cross-repository work". `In Progress` is neither.

## Tests today

- `tests/dispatch_group.rs:222-266`
  `dependency_graph_builds_dispatchable_and_previously_delivered_sub_issues`
  asserts `In Progress` → `skip_reason = Some("In Progress")` (`:260-265`).
- `tests/dispatch_group.rs:300-324`
  `ready_selection_treats_skipped_blockers_as_satisfied_but_failed_blockers_as_blocking`
  asserts a skipped blocker unblocks its dependent.

The two are **not combined**: no test has an `In Progress` blocker *with* a
dependent, so the unblock consequence is implied by the pair rather than
directly asserted. A fix needs a new test at that combined scenario.

## Options

1. **Block until Done/Merged.** Align with CONTEXT's "must be completed". A
   dependency owned by a human or another agent can then stall the group
   indefinitely - the liveness tension ticket 04 wrestled with, and the reason
   no default `max_cycles` was added there.
2. **Keep skipping non-todo statuses as "already owned" (status quo).** Amend
   CONTEXT to say "completed or otherwise not jjfx-dispatchable", so the glossary
   and the code agree.
3. **Split the classification.** Cross-repository/delivered work unblocks;
   in-flight statuses block, with a reason surfaced. Most faithful to both docs,
   but adds a status distinction that is not in the wire vocabulary
   (`docs/wsg-compatibility-contract.md:66-82`).

## Acceptance Criteria

- [ ] The chosen rule is recorded in `## Comments` (or in CONTEXT).
- [ ] A test pins an in-flight blocker with a dependent.
- [ ] If the code changes, `mise run check` is green.
