# `jjfx help ticket` is an unknown command

Status: ready-for-agent
Type: bug
Blocked by: none

## Problem

The top-level help lists the `ticket` commands:

    $ jjfx help
    ...
    jjfx ticket list [--json] [--label <LABEL>] [--pool <name>]  List every Ticket...
    jjfx ticket reconcile                                        Resolve finished...

and a subcommand's page resolves:

    $ jjfx help ticket list
    jjfx ticket list - List every Ticket the configured source discovers, ...

but the group itself does not:

    $ jjfx help ticket
    Error: unknown command: ticket. Run 'jjfx help' for the command list.

An agent that reads a two-word command in the help and asks for its group page -
the obvious next probe - is told the command does not exist. The same is true of
any other two-word group (`pool` works only because it is also a command in its
own right).

## Acceptance Criteria

- [ ] `jjfx help <group>` for a multi-word group (`ticket`) prints that group's
      subcommands (or the top-level page plus the group's rows), rather than
      "unknown command".
- [ ] `jjfx help <group> <sub>` continues to print the subcommand page.

## References

- `src/cli_help.rs`, `src/cli.rs` (`parse_help`)

## Triage (2026-10-20)

Triaged from `needs-triage` to `ready-for-agent`. The evidence is a **live
reproduction during the second drive**, not a code reading: the drive hit this
while working a real backlog through jjfx, and the report
(`.scratch/dogfood-2/report.md`) records what happened around it.

Independently **confirmed**: `jjfx help ticket` prints
`Error: unknown command: ticket.`

The acceptance criteria are unchanged - they were already specific and
testable, which is why this needed triage rather than a rewrite.
