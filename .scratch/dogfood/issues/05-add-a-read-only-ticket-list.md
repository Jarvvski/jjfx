# Add a read-only `jjfx ticket list`

Status: ready-for-agent
Type: feature
Blocked by: none

## Problem

There is no way to see the Ticket backlog and its readiness without dispatching.
The only place a blocked Ticket is named is stderr during `dispatch --all`:

    dark-mode--02-respect-system: blocked by dark-mode--01-toggle (ready-for-agent)

An operator about to dispatch cannot ask "what is ready, what is blocked, and
by what?" - which is the first question the skill tells them to answer.

## Acceptance Criteria

- [ ] A read-only command lists every Ticket the configured source discovers,
      with its `Status`, its blockers and whether it is currently ready.
- [ ] `--json` emits the same information as structure, with stable field names.
- [ ] It never mutates state - no reconciliation side effects.

## References

- `dispatch --all`'s stderr blocker line
