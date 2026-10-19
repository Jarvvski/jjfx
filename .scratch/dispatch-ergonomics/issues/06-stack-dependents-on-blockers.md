# `--all` must base a dependent on its blocker

Status: resolved
Type: task
Blocked by: none

## Problem

`Blocked by:` is declared in the Ticket, but only the orchestration path passes it to the dispatcher as dependency context (`src/engine/orchestration.rs`). `dispatch --all` passes none, so whether a dependent is based on its blocker's branch is left to the agent to guess - and agents guess differently.

Observed in a test-drive with the chain 01 → 03 and 02 → 04:

- Ticket 03's Worker found `adam/todo-md--01-...` in the shared repository, decided 01 was "already complete and pushed", and rebased itself onto it.
- Ticket 04's Worker decided to "implement standalone over the in-memory store; it composes cleanly with 02 once that lands" and stayed on `main`.

So 04 did not contain 02. Integration then hit a conflict in the one file both tickets rewrote - a conflict jjfx's own ticket graph said should not have existed.

## Solution

Resolve each Ready Ticket's blockers and pass the blocker's branch as dependency context, the way orchestration already does. If a blocker has no branch to base on, refuse the Dispatch and name the blocker, rather than silently basing the dependent on trunk.

## Acceptance Criteria

- [x] Under `--all`, a Ticket whose blocker is resolved is based on that blocker's branch.
- [x] A dependent whose blocker has no branch is refused with a message naming the blocker - or is explicitly based on trunk with that stated.
- [x] Tests cover the stacked and unstacked cases.
- [x] `mise run check` is green.

## References

- `src/engine/orchestration.rs` (dependency context as it exists today)
- `src/cli.rs` (`dispatch_command`, the `--all` path)
- `src/engine/direct_dispatch.rs` (`with_dependency_context`)

## Comments

- 2026-10-16: Filed from the `todo-md` test-drive.
- 2026-10-16: Resolved. Ready discovery now carries each Ticket's declared Blockers: `TicketQueryRequest::ReadyTickets` asks the source for `blockedBy`, `RawTicket` parses them into `ReadyTickets::blockers(id)` (a Ticket with none is absent), the local markdown source resolves each `Blocked by:` number to its sibling Ticket id, and an unusable Blocker id is a diagnostic that excludes the dependent rather than a silently dropped dependency. `cli::dispatch_command`'s `--all` path resolves each ready Ticket's Blockers to branches through the same `resolve_ticket_branch` orchestration uses (bookmark lookup by Ticket id, shared via `crate::engine`) and attaches a `DispatchDependencyContext` with `with_dependency_context`, so a dependent's Worker Workspace is based on its Blocker's branch and its Pull Request targets that branch. A Blocker with no branch - including one merged to trunk, which carries `main` - is not used as a base; it is reported and the dependent is stated as based on trunk (or on the remaining stacked branches), which is AC2's explicit-trunk alternative. The pure `dependency_basis(blockers, branch_of)` seam in `src/engine/orchestration.rs` is unit-tested for the stacked, unstacked, merged, and mixed cases; `tests/ticket_discovery.rs` covers a declared Blocker, none, and an unusable one; and `src/engine/local_tickets.rs` covers a ready dependent reporting its resolved sibling. `mise run check` is green and `momo add` records the change.

## Resolution

Reconciled Mon 19 Oct 2026 against the merged code at `6d6f3b689e0e`.

This ticket was marked resolved without its acceptance boxes ticked. Every criterion was re-verified by an independent read-only review of the code and tests on `main`, and `mise run check` was re-run green on `6d6f3b689e0e`. The reconciliation recorded no new work; it confirms what shipped.
