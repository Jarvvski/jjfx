# Add a read-only `jjfx ticket list`

Status: resolved
Type: feature
Blocked by: none

## Problem

There is no way to see the Ticket backlog and its readiness without dispatching.
The only place a blocked Ticket is named is stderr during `dispatch --all`:

    dark-mode--02-respect-system: blocked by dark-mode--01-toggle (ready-for-agent)

An operator about to dispatch cannot ask "what is ready, what is blocked, and
by what?" - which is the first question the skill tells them to answer.

## Acceptance Criteria

- [x] A read-only command lists every Ticket the configured source discovers,
      with its `Status`, its blockers and whether it is currently ready.
- [x] `--json` emits the same information as structure, with stable field names.
- [x] It never mutates state - no reconciliation side effects.

## References

- `dispatch --all`'s stderr blocker line

## Resolution

`jjfx ticket list [--json] [--label <LABEL>] [--pool <name>]` lists every Ticket
the configured source discovers, one row per Ticket: its `Status`, its state
(`ready` / `blocked` / `done`), its blockers and, when it is blocked, the reason
a Dispatch would print. `--json` emits the same as `{"lanes": [...], "tickets":
[...]}` - each Ticket carrying `ticket`, `title`, `status`, `state`, `ready`,
`blockers` and `reason` - and the `lanes` array names every lane searched, so an
empty list still says where it looked.

Readiness is not re-derived. The lanes are the lanes a `dispatch --all` would
fill (`discovery_lanes`, shared with `dispatch_command`), each lane's filter is
built by `lane_filter` (shared with the `--all` paths), and a local Ticket's
status, blockers and verdict all come from one `LocalTickets::backlog`, which the
Dispatch discovery payload is now built on too. The two therefore cannot
disagree about whether a Ticket is ready.

It does not mutate. It never reconciles Runs, writes queue state or touches a
Worker: `tests/ticket_list.rs` fingerprints every file under the repository,
runs `ticket list` over a fixture holding a finished Run a reconcile would
resolve, and asserts the tree is byte-identical - then asserts the fixture is
reconcile-sensitive by showing `ticket reconcile` *does* rewrite it. The command
also needs no terminal and no `jj` on `PATH`, which a second test pins.
