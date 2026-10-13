# Make the forge pipeline report failures instead of empty results

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

The forge pipeline repeatedly converts failures into "nothing to do", and its
conflict/push detection is unsound. This breaks ADR-0006's promise that the work
lifecycle "is queried live from `jj` and `gh`" and "cannot rot out of sync with
reality".

- **I1 - every `gh` failure looks like "no origin remote".**
  `forge.rs:592-607` does `default_branch(&slug).ok()` and
  `Ok(None) | Err(_) => None`. Auth failure, rate limit, network error, or a
  non-GitHub origin all skip the PR step with `pr: no origin remote`
  (`:646`), hiding the real cause.
- **I2 - PR read failures look like "no PRs".** `prs.rs:61-63` returns
  `Vec::new()` on any failure, so a merged workspace shows `Pushed` instead of
  `Merged` (`work.rs:142-148`). `--limit 100` over `--state all` also drops
  older PRs. The port cannot distinguish "empty" from "failed" because
  `WorkReads::pull_requests` returns a bare `Vec`.
- **I3 - conflict detection only inspects `@`.** `forge.rs:313-328` runs
  `@ & conflicts()`; a conflict on a lower commit is invisible, `jj rebase`
  exits 0 on conflict, and `PUSH_REVS` pushes the rest.
- **I4 - push result sniffed from English.** `pushed_something`
  (`forge.rs:36-39`, `:339-346`) matches `"Nothing changed."`; a genuinely
  failed push (non-fast-forward, multiple remotes) exits non-zero and reads as
  "nothing to push", then the pipeline continues.

## Fix

1. Make `WorkReads::pull_requests` return `Result`, and thread the failure
   through so "gh failed" is distinct from "no PRs". Paginate beyond 100.
2. Replace `.ok()` on `default_branch` with a typed error that surfaces the
   cause; only "no GitHub origin" skips the PR step.
3. Detect conflicts across the whole chain, not just `@`, and treat a
   `jj rebase` that produced conflicts as a failure.
4. Type the push outcome (pushed / nothing / failed) from the exit code and
   revset, not stdout text.

## Commits

1. `WorkReads::pull_requests` -> `Result`; pagination; callers.
2. Typed `default_branch` error; surface it in the forge step result.
3. Chain-wide conflict detection; fail the weld on conflict.
4. Typed push outcome; fail the pipeline on a failed push.
5. Tests with stubbed `gh`/`jj` for each failure mode.
6. CHANGELOG entry `(fix)`.

## Acceptance Criteria

- [ ] An unauthenticated / rate-limited `gh` surfaces its cause, not "no origin
      remote".
- [ ] A merged PR is not misreported as `Pushed` when `gh` fails.
- [ ] A conflict below `@` fails the weld instead of pushing.
- [ ] A failed push fails the pipeline.
- [ ] `mise run check` is green.

## Out of Scope

- Moving the whole forge read path to jj-lib (ticket 21).
