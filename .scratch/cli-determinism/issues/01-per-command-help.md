# `jjfx help <command>` ignores its argument

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

`jjfx help dispatch` prints exactly what `jjfx help` prints - the top-level command list. There is no per-command help anywhere in the CLI, so the only way to learn a command's arguments and flags is to read `src/cli.rs` or the README table. An agent driving jjfx therefore cannot discover the flags from the tool at all: the installable `skills/jjfx-dispatch` skill has to hard-code them, and they go stale - its `dispatch` flag table had drifted from the parser by the time it was last corrected.

The top-level help is also missing the Lifecycle group: `jjfx hooks install` and `jjfx hooks status` exist and are documented in `README.md` ("Lifecycle | `hooks install`, `hooks status`"), but neither appears in the help text, so nothing tells a reader they exist.

## Evidence

- `src/cli.rs`: `parse` maps `"help" | "-h" | "--help"` to `Command::Help` and discards the rest of argv, so the argument is never read.
- `jjfx help dispatch`, `jjfx help pool`, `jjfx help open-pr` all print the same `HELP` string.
- `src/hooks.rs` (`//! jjfx hooks install / jjfx hooks status: manage lifecycle integrations`) - the commands exist; the help text omits them.
- `README.md` §"Command surface" lists the Lifecycle group the help is missing.

## Solution

Give every command a help page - usage line, arguments, flags - reachable as `jjfx help <command>`, and add the Lifecycle group to the top-level list.

Derive the flag text from the same definitions the parser reads, so the two cannot drift: a flag that exists in `parse_*` and not in its help page (or vice versa) should be a test failure, not a review catch.

## Acceptance Criteria

- [ ] `jjfx help <command>` prints that command's usage, arguments and flags, for every command the top-level help lists.
- [ ] `jjfx help <unknown>` fails loudly, naming the unknown command.
- [ ] The top-level help lists `hooks install` and `hooks status`.
- [ ] A test asserts every command named in the top-level help has a help page, so a new command without one fails the suite.
- [ ] `mise run check` is green.

## Out of Scope

- Shell completion (`jjfx completion`) - unchanged.
- Rewriting the top-level help's wording beyond adding the missing group.

## References

- `src/cli.rs` (`parse`, `HELP`, the `parse_*` functions)
- `src/hooks.rs`
- `README.md` §"Command surface"
- `skills/jjfx-dispatch/SKILL.md` (the flag table that goes stale when the CLI cannot be asked)

## Comments

- 2026-10-16: Filed from the `jjfx-dispatch` skill pass, which had to hard-code the flag surface because the CLI could not be asked for it.
