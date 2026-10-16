# Orchestration must work for a local feature Parent

Status: ready-for-agent
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

- [ ] `jjfx dispatch <feature>` orchestrates a local feature's Tickets in dependency order.
- [ ] A Parent with no Ticket file of its own is not an error.
- [ ] No error message mentions Linear when `source = "local"`.
- [ ] Tests cover orchestrating a local feature.
- [ ] `mise run check` is green.

## References

- `src/cli.rs:1701-1710` (`fetch_ticket_details`, the Linear hint)
- `src/engine/local_tickets.rs` (`details_payload`, `dependency_payload`)
- `src/engine/dispatch_group.rs`, `src/engine/orchestration.rs`
- `README.md` § Quickstart

## Comments

- 2026-10-16: Filed from the `todo-md` test-drive, where this made the advertised flow unusable.
