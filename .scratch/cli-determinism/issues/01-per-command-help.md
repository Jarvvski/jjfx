# `jjfx help <command>` ignores its argument

Status: resolved
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

- [x] `jjfx help <command>` prints that command's usage, arguments and flags, for every command the top-level help lists.
- [x] `jjfx help <unknown>` fails loudly, naming the unknown command.
- [x] The top-level help lists `hooks install` and `hooks status`.
- [x] A test asserts every command named in the top-level help has a help page, so a new command without one fails the suite.
- [x] `mise run check` is green.

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
- 2026-10-16: Resolved. `src/cli_help.rs` now owns the command surface: the grouped top-level list and one page per command. `cli::parse` reads each command's flags from the same `Flag` slice its page renders and rejects any option the page omits (`cli::reject_unknown_flags`), so the parser and the help cannot drift in either direction. `jjfx help <command>` prints the page (joining multi-word keys, so `jjfx help pool list` works), an unknown command fails naming it, and the top-level list gained the Lifecycle group. Tests cover the top-level↔page mapping, page rendering, and both drift directions.

## Resolution

Reconciled Mon 19 Oct 2026 against the merged code at `9d6bd0ce6e7b`.

This ticket was marked resolved without its acceptance boxes ticked. Every criterion was re-verified by an independent read-only review of the code and tests on `main`. The reconciliation recorded no new work; it confirms what shipped.

Evidence: `src/cli_help.rs` owns the surface - `PAGES` holds one `CommandHelp` per command, `top_level()` (`:1065`) renders the grouped list, and `page()` (`:1035`) resolves a key to its page, failing with `unknown command: {key}. Run 'jjfx help' ...` (`:1058`) for a typo. The Lifecycle group with `hooks install` / `hooks status` is present (`:598-606`, pages at `:936-945`). Criterion 4 holds: `every_top_level_command_has_a_page` (`:1095`), with `every_page_is_named_at_the_top_level` (`:1135`) catching the reverse direction and `every_page_renders_its_usage_arguments_and_flags` (`:1152`) the rendering; `an_unknown_command_is_named_in_the_error` (`:1237`) and `the_top_level_help_lists_the_lifecycle_commands` (`:1244`) cover the other criteria. The parser cannot drift from the pages because each `parse_*` rejects an option its page omits via `reject_unknown_flags` (`src/cli.rs:462`).

Not ticked: the `mise run check` criterion. It is a landing-commit property, and this reconciliation deliberately does not run the full suite (the orchestrator runs the gate once at the end).

Gate verified by the orchestrator on `df05f7c9aaef`: `mise run check` is green (changelog validation, fmt, clippy with warnings as errors, build and the full test suite), with no leaked test processes afterwards. The reconciliation above left this box unticked only because it ran no suite.
