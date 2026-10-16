# Take a Ticket from free text, a file path, or stdin

Status: resolved
Type: task
Blocked by: none

## Problem

`jjfx dispatch` can only dispatch a Ticket the configured source can resolve: Linear, or a file in the `.scratch/<feature>/issues/<NN>-<slug>.md` convention. Anything else has to be dressed up as a tracker item first.

That is backwards for the case that actually comes up - "here is the thing I want done" - and it means a repository that has never been set up for jjfx cannot dispatch at all. The default source is `linear` (`src/config.rs`), which wants an API key, so out of the box the tool does not work.

Two walls, both hit in a real test-drive of a small repository:

- **There is no way to hand a Worker a paragraph or a file.** You must create a ticket file, and it must sit in the `.scratch/<feature>/issues/` convention or it is invisible.
- **The Ticket source is user-global.** `.jjfx/config.toml` accepts only `[checks]` (`RepoConfig`, `deny_unknown_fields`), so a repository cannot declare "my tickets are local". The only lever is `${XDG_CONFIG_HOME:-~/.config}/jjfx/config.toml`, and there is no `--config` flag or environment override. The test-drive had to fake `XDG_CONFIG_HOME` with a symlink farm to avoid repointing the owner's global config away from Linear.

## Solution

Let `dispatch` take a Ticket from somewhere other than a tracker:

- `jjfx dispatch --text "<free text>"` - a one-off Ticket whose body is the text.
- `jjfx dispatch --from-file <path>` - a Ticket from any markdown file, wherever it lives, not only the `.scratch` convention.
- `jjfx dispatch -` (or `--from-file -`) - read it from stdin.

And make the defaults work with no configuration:

- A Ticket supplied by flag or stdin must not need a source at all.
- A repository with no `[tickets]` section and no `.scratch/` tree must still be able to dispatch ad-hoc work, rather than failing on a Linear key it does not have.

Design notes, not requirements:

- **Identity matters.** The Worker prompt and the pool state name the Ticket. A local Ticket's id is path-derived (`<feature>--<NN>-<slug>`). An ad-hoc Ticket needs a stable, obvious id: an optional `--id`, with a default derived from the file stem or the first words of the text, is fine.
- **Keep it orthogonal.** These are inputs to the same Direct Dispatch path, not a third source implementation bolted alongside the other two.
- **A file Ticket should carry title and body** the way `LocalTicket::parse` does (first `# ` heading, rest as body), so the Worker prompt is identical whichever input was used.

## Acceptance Criteria

- [x] `jjfx dispatch --text "..."` dispatches a Worker with that text inlined, with no tracker configured.
- [x] `jjfx dispatch --from-file <path>` works for a markdown file outside `.scratch`.
- [x] Reading the Ticket from stdin works.
- [x] A repository with no `[tickets]` section and no `.scratch/` can dispatch an ad-hoc Ticket without a Linear key.
- [x] `jjfx help dispatch` documents the new flags.
- [x] Tests cover each input, and the no-config path.
- [x] `mise run check` is green.

## References

- `src/cli.rs` (`DispatchArgs`, `parse_dispatch`, `dispatch_command`)
- `src/ticket_source.rs` (`configured_ticket_query`)
- `src/engine/local_tickets.rs` (the local convention, `LocalTicket::parse`)
- `src/config.rs` (`TicketsConfig`, `TicketSource`, `RepoConfig`)
- `docs/agents/issue-tracker.md`

## Comments

- 2026-10-16: Filed from the `todo-md` test-drive.
- 2026-10-16: Resolved. `dispatch` gains `--text TEXT`, `--from-file PATH`, and a positional `-` (with `--from-file -` as an alias) to read the Ticket from stdin; `--id ID` names it, defaulting to `ad-hoc--<slug>` derived from the file stem or the first words of the text. The new `src/adhoc_ticket.rs` turns free text, a file, or stdin into the same `Ticket` + `TicketDetails` a configured source would supply (first `# ` heading is the title, the rest the body, matching `LocalTicket::parse`), and `cli::dispatch_command` dispatches that straight down the existing Direct Dispatch path - it never calls `configured_ticket_query`, so an ad-hoc Dispatch works with no `[tickets]` and no `.scratch/`, which is the default-`linear` case that previously failed on a missing API key. It is always a single direct request: an ad-hoc Ticket has no Parent to orchestrate and no file to write back. `--all`, explicit positional Tickets, and a second input are rejected as ambiguous; `--budget` is allowed (the single-Ticket orchestration guard does not apply). `cli_help::DISPATCH_FLAGS` documents all three flags and the dispatch page gains a `-` argument, so `jjfx help dispatch` and the parser stay in lockstep. `reject_unknown_flags` now treats a bare `-` as the stdin marker rather than an unknown option. Covered by unit tests for text/file/id derivation and the `--text`/`--from-file`/stdin parse, and by `tests/adhoc_ticket.rs`, which drives the real binary in a bare jj repo (no `[tickets]`, no `.scratch/`) for each input and asserts the ad-hoc id reaches the Pool without a tracker. `momo add` records the change.
