# Port the persistent orchestration runner

Status: ready-for-agent

## Parent

epics/C-dispatch-and-orchestration.md

## Problem Statement

The Dispatch Group model cannot make progress by itself. A live runner must reconcile Worker outcomes, reset failed attempts, claim capacity, launch ready Sub-issues on correct bases, persist every transition, and resume after restart without duplicate Runs.

## Solution

Implement an orchestration runner that drives one Dispatch Group against narrow Worker Pool and Direct Dispatch interfaces. Support foreground watching and detached internal execution, one retry, branch revalidation, bounded polling, status events, and restart recovery.

## Commits

1. Define an orchestration execution interface over Worker reads, Reset, claim, and Direct Dispatch.
2. Implement one deterministic advance operation that reconciles existing Workers before launching new work.
3. Persist each changed Dispatch Group before waiting or emitting completion.
4. Implement retry by Resetting the failed Worker and returning the Sub-issue to dispatchable state.
5. Claim and launch every Ready Sub-issue allowed by current capacity.
6. Pass dependency-derived base branches and context into Direct Dispatch.
7. Revalidate persisted branch references before resuming a group.
8. Add foreground progress events and terminal summaries without terminal formatting.
9. Add a detached orchestration entrypoint with one active runner per Parent Ticket.
10. Resume a non-terminal Go-created Dispatch Group after process restart.
11. Ensure errors release any placeholder Reservation made before graph discovery.

## Decision Document

- The runner is an application module over the pure Dispatch Group aggregate.
- Progress is persisted after every mutation.
- Worker state is authoritative for a dispatched Sub-issue's current Run outcome.
- A runner restart must be idempotent.
- Frontends render progress events and decide foreground versus detached use.
- Capacity shortage waits and retries; it does not fail the Dispatch Group.

## Testing Decisions

Drive the runner with a fake world for deterministic wave and failure tests, then add temporary-Repository integration tests for persistence and restart. Cover process interruption between claim, launch, and save; reset failure; missing Worker; missing branch; and simultaneous runner attempts.

## Acceptance Criteria

- [ ] Dependency waves launch only when all Blockers unblock them.
- [ ] A failed Run retries once and then becomes terminal.
- [ ] Restart resumes without duplicate launches.
- [ ] Every state mutation is durable before the next wait cycle.
- [ ] Existing Go-created live groups can continue under Rust.
- [ ] `mise run check` is green.

## Out of Scope

- CLI flag parsing
- TUI rendering
- More than one automatic retry
- Cross-Parent dependency orchestration

## Blocked by

- issues/11-port-direct-dispatch.md
- issues/12-port-dispatch-group-model.md
