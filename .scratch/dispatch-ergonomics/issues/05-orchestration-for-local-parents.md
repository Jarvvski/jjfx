# Orchestration must work for a local feature Parent

Status: resolved
Type: task
Blocked by: none

## Problem

The README leads with the feature-as-Parent flow:

```sh
jjfx dispatch todo-md        # the feature as a Parent: its Tickets in dependency order
```

For local Tickets it does not work, and the hint names the wrong tracker:

```
$ jjfx dispatch todo-md --fg
Error: cannot fetch Ticket todo-md to inline into the Dispatch prompt; ensure the pool runtime can reach Linear

Caused by:
    Ticket discovery query failed: query failed: local Ticket todo-md was not found under /Users/jarvis/Code/personal/todo-md/.scratch
```

Two bugs in one message:

1. **A local feature Parent is a directory, not a Ticket file.** `LocalTickets::details_payload` (`src/engine/local_tickets.rs`) returns a hard error when the id is not a file. For a Parent it should be `None`, so orchestration proceeds to the Parent's children - which `LocalTickets::dependency_payload` already knows how to enumerate.
2. **The message is hard-coded to Linear** (`src/cli.rs:1701-1710`) regardless of `[tickets] source`. In a repository configured `source = "local"` the word "Linear" is simply wrong, and it sends the reader to the wrong subsystem.

Note the background path hides this entirely - the same call in background mode prints `orchestrating` and exits 0. See the sibling ticket on dispatch reporting success.

## Solution

- Make a Parent id resolve to `None` details rather than an error, so orchestration falls through to the Dispatch Group.
- Key the hint off the configured source; a local source should name the root it searched.
- Cover the local Parent path with a test - the dependency graph, and the dispatch.

## Acceptance Criteria

- [x] `jjfx dispatch <feature>` orchestrates a local feature's Tickets in dependency order.
- [x] A Parent with no Ticket file of its own is not an error.
- [x] No error message mentions Linear when `source = "local"`.
- [x] Tests cover orchestrating a local feature.
- [x] `mise run check` is green.

## References

- `src/cli.rs:1701-1710` (`fetch_ticket_details`, the Linear hint)
- `src/engine/local_tickets.rs` (`details_payload`, `dependency_payload`)
- `src/engine/dispatch_group.rs`, `src/engine/orchestration.rs`
- `README.md` § Quickstart

## Comments

- 2026-10-16: Filed from the `todo-md` test-drive, where this made the advertised flow unusable.
- 2026-10-16: Resolved. `LocalTickets::details_payload` now returns the JSON `null` sentinel when the id names a feature directory (the local Parent) rather than a Ticket file, and `TicketDiscovery::ticket_details` reads that as `Ok(None)` - so orchestration falls through to `dependency_payload` and the feature's Tickets in dependency order. An id that is neither a Ticket nor a feature stays a hard error, so a mistyped Ticket is still caught rather than silently dispatched without its body. The fetch-details hint is now keyed off the configured source through a new `ticket_source_hint`: `source = "local"` names the root it searched (`.scratch`), `linear` keeps the Linear wording, so no message mentions Linear in a local repository. Tests: `tests/local_tickets.rs` covers a feature Parent resolving to no details while an unknown id still fails; `tests/orchestration.rs` discovers a local feature Parent into a real Dispatch Group with its child's blocker; `tests/dispatch_outcomes.rs` drives the real binary for the background handoff, and its pre-persisted terminal group (the ticket-04 shortcut) is replaced with real local discovery, plus the unresolvable-Ticket case asserts the hint names `.scratch` and not Linear. `mise run check` is green and `momo fix` records the change.
