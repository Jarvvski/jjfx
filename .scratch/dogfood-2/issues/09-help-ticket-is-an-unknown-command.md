# `jjfx help ticket` is an unknown command

Status: needs-triage
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
