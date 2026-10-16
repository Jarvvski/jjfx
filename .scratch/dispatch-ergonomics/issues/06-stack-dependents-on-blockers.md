# `--all` must base a dependent on its blocker

Status: ready-for-agent
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

- [ ] Under `--all`, a Ticket whose blocker is resolved is based on that blocker's branch.
- [ ] A dependent whose blocker has no branch is refused with a message naming the blocker - or is explicitly based on trunk with that stated.
- [ ] Tests cover the stacked and unstacked cases.
- [ ] `mise run check` is green.

## References

- `src/engine/orchestration.rs` (dependency context as it exists today)
- `src/cli.rs` (`dispatch_command`, the `--all` path)
- `src/engine/direct_dispatch.rs` (`with_dependency_context`)

## Comments

- 2026-10-16: Filed from the `todo-md` test-drive.
