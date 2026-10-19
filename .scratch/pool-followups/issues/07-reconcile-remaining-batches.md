# Reconcile the remaining batches' unticked acceptance criteria

Type: task
Status: ready-for-agent
Blocked by: none

## Problem

The two most recent batches were reconciled in PR #92: thirteen tickets were marked `resolved` with their acceptance boxes unticked and no Resolution note; twelve were confirmed by an independent re-read of the merged code and one was re-opened.

The same drift runs through every older batch. `.scratch/bug-hunt`, `cli-determinism`, `help-menu`, `jj-lib-reads`, `opinionated-loop`, `pi-support`, `rebuild`, `release-workflow` and `remove-pi` all carry `resolved` tickets with unticked boxes, so `Status: resolved` cannot be trusted as an audit trail.

That is not cosmetic. The worked example: `.scratch/bug-hunt/issues/35` and `42` are resolved tickets about load-sensitive and flaky tests, and two tests from those classes flaked again under CPU load during the named-pools work - a new `engine::locks` test, and `tests/adhoc_ticket.rs::a_markdown_file_outside_scratch_dispatches`, which failed once with empty stdout and passed every re-run. A resolved ticket whose class recurs is exactly what an unreconciled record hides.

## Solution

Work batch by batch, oldest first, the way the two recent batches were done: verify each unticked criterion against the merged code by an independent read-only review, then tick it, or re-open the ticket if it does not hold. Record a short Resolution note either way, so the next reader knows the tick was earned.

File the flakes against the existing bug-hunt tickets rather than as new work, unless the reconciliation shows those tickets do not actually cover them - in which case re-open them.

## Acceptance Criteria

- [ ] Every ticket marked `resolved` has either all its criteria ticked or a Resolution note explaining what remains.
- [ ] Any criterion that does not hold has re-opened its ticket.
- [ ] The two flaking tests are either covered by a live ticket or filed as one.
- [ ] No code changes are made - this is a record reconciliation.

## References

- PR #92 (the method, and the two batches already reconciled)
- `.scratch/bug-hunt/issues/35-load-sensitive-helper-deadlines.md`, `42-flaky-process-and-exec-tests.md`
