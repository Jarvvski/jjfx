# The bounded review loop has no caller, and `jjfx review --fg` says it does

Status: ready-for-agent
Type: design
Blocked by: none

## Problem

`harness/04` put the review bound in `ReviewLoop` (`src/engine/reviewer.rs`) and
`harness/12` wired it through the decision log in `RecordedReviewLoop`
(`src/engine/review_run.rs`), so a repeated pass replays rather than reaching the
model. Both are real and tested.

But **nothing in the binary drives that loop.** `RecordedReviewLoop` has no
caller outside its own tests:

    $ grep -rn RecordedReviewLoop src/
    src/engine/mod.rs:28:mod review_run;
    src/engine/mod.rs:87:pub use review_run::{REPLAYED_DETAIL, RecordedReviewLoop, ReviewSubject};
    src/engine/review_run.rs:60:pub struct RecordedReviewLoop<S> {   # and its tests

`WorkerActions::review` (`src/engine/worker_actions.rs:759`) builds a prompt that
inlines the PR's review state and calls `follow_up` - one agent Run, no
`ReviewLoop`, no pass cap, no progress rule, no replay. Yet the help page claims
the opposite:

    $ jjfx help review
    jjfx review - Address a Worker's pull-request review comments
    Usage: jjfx review <worker> [--fg|--bg]
    Flags:
      --fg    Run the full bounded review loop in the foreground
      --bg    Run a single background review pass (default)

`--fg` only selects foreground vs background for the single follow-up Run; it
does not run `ReviewLoop`.

**This answers the second drive's open question.** The map asked whether an agent
working a backlog through jjfx ever repeats a review dimension, and whether
anything notices if it does. It cannot repeat one through the CLI, because **it
never runs a dimension at all** - there is no command that reaches `ReviewLoop`,
so the seven-dimensions-recurring-8-11x reduction is unreachable from the
operator surface. Nothing notices a repeat because nothing runs a pass to repeat.
(`harness/12` disclosed the same gap one layer out; this is that gap confirmed
end to end against the built binary.)

## Acceptance Criteria

- [ ] Either `jjfx review` drives the bounded, decision-logged loop (the
      `--fg` behaviour its help advertises), or the help and the docs stop
      claiming it does and the loop's lack of a caller is stated where an
      operator will see it.
- [ ] If it is wired: a pass already recorded replays instead of reaching the
      reviewer, the pass cap and progress rule apply, and the escalation is
      visible; `jjfx status --json`'s `queue` shows a replayed pass.

## References

- `src/engine/review_run.rs` (`RecordedReviewLoop`), `src/engine/reviewer.rs`
  (`ReviewLoop`)
- `src/engine/worker_actions.rs:759` (`WorkerActions::review`)
- `.scratch/harness/issues/04-bound-the-review-loop.md`,
  `.scratch/harness/issues/09-idempotent-steps.md`,
  `.scratch/harness/issues/12-wire-the-decision-log.md`
- `.scratch/dogfood-2/map.md` (the question this answers)

## Triage (2026-10-20)

Triaged from `needs-triage` to `ready-for-agent`. The evidence is a **live
reproduction during the second drive**, not a code reading: the drive hit this
while working a real backlog through jjfx, and the report
(`.scratch/dogfood-2/report.md`) records what happened around it.

Independently **confirmed**: `jjfx help review` prints `--fg  Run the full
bounded review loop in the foreground`, and no command reaches `ReviewLoop`.

The acceptance criteria are unchanged - they were already specific and
testable, which is why this needed triage rather than a rewrite.
