# Make the forge pipeline report failures instead of empty results

Status: resolved
Type: task
Blocked by: none

## Problem

Four places turned a failure into a success-shaped empty result, so a broken
forge looked like a clean one:

- **I1** `run_pipeline` used `github.default_branch(&slug).ok()`, so a failed
  `gh repo view` collapsed into "no origin remote".
- **I2** `prs::list` returned an empty `Vec` on any `gh` failure, so a failed
  `gh pr list` collapsed into "no Pull Requests".
- **I3** `has_conflict` only inspected `@`, so a weld that buried a conflict in a
  lower commit still pushed and opened a Pull Request over broken commits.
- **I4** `pushed_something` returned `false` both for "nothing to push" and for a
  *failed* push, so a rejected push read as a no-op and the pipeline carried on.

## Fix

I1, I3 and I4 are fixed here (all in `src/forge.rs`):

- **I4** `JjAdapter::push` returns a typed `PushOutcome`
  (`Pushed` / `Nothing` / `Failed(message)`). The no-op marker only counts on a
  zero exit; a non-zero exit is a failure. A failed push marks the Push step
  skipped with jj's message and **stops** that target before the Pull Request
  step, because the remote was not updated.
- **I3** `has_conflict` now checks the whole mutable chain
  (`(::@ ~ trunk()) & conflicts()`), and the pipeline re-checks after weld: a
  rebase that produces a conflict still exits zero, so the chain is verified
  before pushing.
- **I1** the Pull Request context is now `Result<Option<..>>`; a failed
  `origin_slug` or `default_branch` is reported as `pr: <error>` instead of
  `pr: no origin remote`.

I2 is split into ticket 29: `prs::list` degrades to "no PR info" on purpose, and
changing that changes what the work list *renders* when `gh` is down, which
deserves its own decision and test.

## Commits

1. `Report forge step failures instead of clean skips` - `PushOutcome`, the
   chain-wide conflict check, the surfaced Pull Request context, three tests, and
   a CHANGELOG `(fix)`.

## Acceptance Criteria

- [x] A failed push is reported and does not open a Pull Request.
- [x] A conflict below `@` stops the pipeline before push.
- [x] A failed `gh` default-branch lookup is surfaced.
- [x] `mise run check` is green.

## Out of Scope

- I2 (`prs::list` failure semantics) - ticket 29.
