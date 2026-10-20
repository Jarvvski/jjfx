# The documented `open-pr` contradicts the tool, and nothing holds the help to the parser

Status: resolved
Type: bug
Blocked by: none

## Problem

Found while correcting `skills/jjfx-dispatch/SKILL.md` (`dogfood/07`). Three
user-facing statements about `open-pr` disagree with the code:

- `src/cli_help.rs`'s help page for `open-pr` carries the hand-written summary
  *"Weld a Worker's chain onto trunk and open its Pull Request"*.
- The unreleased `CHANGELOG.md` entry says `open-pr` runs the build check.
- The code (`open_pr_command` -> `WorkerActions::open_pull_request`) runs
  `gh -R <repo> pr view <branch> --web`. It does not weld and it does not run the
  check; the weld/fix/check/push pipeline is the forge, started from the TUI.

The skill was corrected. These two are Rust and changelog, so they were left
alone and filed here rather than edited in a docs-only change.

## Why this is worth more than a string fix

Every command's help page is **derived from the same definitions the parser
reads**, so its *flags* cannot drift. The **summary** is hand-written, and
nothing holds it to anything - which is how it came to describe a different
command. The changelog has the same property: it is prose about behaviour that
no test compares to the behaviour.

## Acceptance Criteria

- [x] `open-pr`'s help summary describes what it does, and the changelog entry
      does too - or the entry is corrected to name the forge instead.
- [x] A test holds the help pages to the parser: every flag on a page is
      accepted by the parser, and every accepted flag appears on the page - the
      promise the help system already makes, made checkable. If that is too
      broad to be honest, say so and do what is.
- [x] Check the other commands' summaries for the same drift while you are
      there, and report what you found. Fixing them is in scope; saying they are
      fine is also a result.

## References

- `skills/jjfx-dispatch/SKILL.md` (corrected by `dogfood/07`)
- `src/cli_help.rs`, `src/cli.rs` (`open_pr_command`)
- `tests/worker_actions.rs` (`open_pull_request_still_reaches_gh_with_a_configured_gh_repo`)
- Found by `dogfood/07` (#140)

## Resolution

Every statement was checked against the code, not taken on trust.

**The three statements.**

- The `open-pr` help summary said *"Weld a Worker's chain onto trunk and open
  its Pull Request"*. It now says *"Open a Worker's Pull Request in the
  browser"* - what `open_pr_command` does: resolve the Worker's bookmark and the
  Pool's `gh_repo`, then `gh -R <repo> pr view <branch> --web`. The weld, `jj
  fix`, check and push are the forge's steps, not this verb's.
- The unreleased changelog entry said the skill covers the repository build
  check "that `open-pr` runs before pushing". The claim is true of the *forge*,
  so the sentence now names it: "...and the repository build check the forge
  runs before pushing." Nothing true was deleted - the statement was attributed
  to the thing that does it. This is a correction of an existing entry, not a
  new one, so no `momo` command applies: `momo break|add|fix` only append and
  momo has no edit/amend subcommand, so the entry was corrected in place. The
  version was not touched.
- The code is unchanged.

**The test.** `the_parser_accepts_exactly_the_flags_a_page_documents` holds the
help pages to the parser in both directions, for every page that declares a
flag. It collects every flag spelling any page defines (long and short, with
whether it takes a value) and probes the parser's public `parse` with each
spelling on each command; a spelling is accepted when `parse` does not reject it
as an unknown option. It then asserts accepted == documented. Probing `parse` -
rather than comparing the page's flag slice to the parser's own, which would be
tautological - is what makes it able to fail: a `parse_*` that stopped
validating against the page's slice would reject a page flag or accept an
undocumented one. Verified by mutating `parse_add` to validate against
`RM_FLAGS`, which failed the test naming `add --revision`.

It is scoped to the pages that declare flags, and that narrowing is the honest
one: a command with no flags parses a bare positional and never inspects
options, so `jjfx open-pr --json` is accepted as a Worker *named* `--json`, not
as a flag - there is no accepted-flag set to hold such a page to. The test
replaces the two weaker ones it subsumes: `documented_flags_are_accepted_by_
the_parser` probed only long spellings and only one direction, and
`an_undocumented_flag_is_rejected_by_the_parser` probed one synthetic flag.

**The other summaries.** Every other page summary was read against its command.
One more had drifted the same way: `pool list` said *"Show every pool, and the
`default` pool's Workers"*, but `render_pool_workers` iterates every pool and
tags each row with a POOL column, so it shows *each* pool's Workers; the summary
now says so. The rest describe what their commands do. Two are broad rather
than wrong and were left alone: `pool` ("Manage the Repository's Worker Pools" -
a bare `pool` lists, `pool <N>` resizes `default`) and `colocation` ("Enable or
disable Git colocation" - the verb also takes `status`). Neither describes a
different command, unlike the `open-pr` summary this ticket filed.

`mise run check` passes. A load-sensitive `store` test failed once under a
sibling worker's load and passed on re-run.
