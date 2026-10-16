# The ready set must say why it is empty

Status: ready-for-agent
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

- [ ] An empty ready set names the Tickets that were skipped and why.
- [ ] A blocked Ticket's diagnostic names the blocker and the blocker's status.
- [ ] The information is present in `--json`, not only the prose.
- [ ] Tests cover blocked, malformed, and genuinely-empty cases.
- [ ] `mise run check` is green.

## References

- `src/engine/local_tickets.rs` (`is_ready`, `malformation`, `malformed_entry`)
- `src/engine/ticket.rs` (`ReadyTickets::diagnostics`)
- `src/cli.rs:1576-1583` (the empty-ready branch)

## Comments

- 2026-10-16: Filed from the `todo-md` test-drive.
