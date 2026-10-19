# The ready set must say why it is empty

Status: resolved
Type: task
Blocked by: none

## Problem

```
$ jjfx dispatch --all --json
Fetching tickets with label 'ready-for-agent'...
[]
EXIT: 0
```

A bare `[]` when the reason is knowable. In a test-drive this hid a deadlocked chain for several minutes: two Tickets were `claimed` and two more were blocked by them, and nothing said so. The reader has to go and read the ticket files, then the source, to find out whether the empty set means "no tickets" or "no tickets *can* run".

The local source already computes readiness (`LocalTickets::is_ready`) and already has a diagnostics channel for malformed files (`LocalTicketMalformation`, surfaced through `ReadyTickets::diagnostics`). Blocked-not-ready deserves the same treatment.

## Solution

When the ready set is empty - or a Ticket the user named is not ready - report why. Name each not-ready Ticket and the reason: blocked by an unresolved sibling (naming the blocker and its current status), the wrong `Status:`, or malformed.

Do it on both the prose and `--json` paths, so an agent reading JSON can tell "nothing to do" from "nothing can be done".

Something in this shape:

```
No tickets found with label 'ready-for-agent'.
  todo-md--03-edit-items-in-place    blocked by 01 (claimed)
  todo-md--04-filter-and-count-items blocked by 02 (claimed)
```

## Acceptance Criteria

- [x] An empty ready set names the Tickets that were skipped and why.
- [x] A blocked Ticket's diagnostic names the blocker and the blocker's status.
- [x] The information is present in `--json`, not only the prose.
- [x] Tests cover blocked, malformed, and genuinely-empty cases.
- [x] `mise run check` is green.

## References

- `src/engine/local_tickets.rs` (`is_ready`, `malformation`, `malformed_entry`)
- `src/engine/ticket.rs` (`ReadyTickets::diagnostics`)
- `src/cli.rs:1576-1583` (the empty-ready branch)

## Comments

- 2026-10-16: Filed from the `todo-md` test-drive.
- 2026-10-16: Resolved. The local source now reports why it excluded a well-formed Ticket instead of dropping it: `LocalTickets::not_ready_reason` names a Ticket whose `Status:` is not the ready status (`expected status "ready-for-agent", found "claimed"`) or whose `Blocked by:` sibling is unresolved (naming the blocker's id and status, e.g. `blocked by widgets--01-foundation (claimed)`), and `ready_payload` emits those through a new `diagnostics` array on the `ReadyTickets` payload that `TicketDiscovery::ready_tickets` folds into `ReadyTickets::diagnostics`. A resolved Ticket is deliberately not reported, so a genuinely finished repository still reads as nothing to do. `cli::dispatch_command`'s `--all` path renders the diagnostics on both paths: prose prints `No ready tickets with label '<label>':` followed by one line per skipped Ticket, and `--json` emits a `DispatchEntry::Skipped` object (`outcome: "skipped"`, `worker`/`phase`/`pid` null, `detail` the reason) instead of a bare `[]`. Tests cover a blocked Ticket, an unknown blocker, a wrong-status Ticket, a resolved-only repository, and the empty JSON array, plus CLI JSON and prose integration tests. `mise run check` is green and `momo add` records the change.

## Resolution

Reconciled Mon 19 Oct 2026 against the merged code at `6d6f3b689e0e`.

This ticket was marked resolved without its acceptance boxes ticked. Every criterion was re-verified by an independent read-only review of the code and tests on `main`, and `mise run check` was re-run green on `6d6f3b689e0e`. The reconciliation recorded no new work; it confirms what shipped.
