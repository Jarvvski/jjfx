# The documented `open-pr` contradicts the tool, and nothing holds the help to the parser

Status: ready-for-agent
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

- [ ] `open-pr`'s help summary describes what it does, and the changelog entry
      does too - or the entry is corrected to name the forge instead.
- [ ] A test holds the help pages to the parser: every flag on a page is
      accepted by the parser, and every accepted flag appears on the page - the
      promise the help system already makes, made checkable. If that is too
      broad to be honest, say so and do what is.
- [ ] Check the other commands' summaries for the same drift while you are
      there, and report what you found. Fixing them is in scope; saying they are
      fine is also a result.

## References

- `skills/jjfx-dispatch/SKILL.md` (corrected by `dogfood/07`)
- `src/cli_help.rs`, `src/cli.rs` (`open_pr_command`)
- `tests/worker_actions.rs` (`open_pull_request_still_reaches_gh_with_a_configured_gh_repo`)
- Found by `dogfood/07` (#140)
