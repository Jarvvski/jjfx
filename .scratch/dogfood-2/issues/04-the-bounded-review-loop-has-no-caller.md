# The bounded review loop has no caller, and `jjfx review --fg` says it does

Status: resolved
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

- [x] Either `jjfx review` drives the bounded, decision-logged loop (the
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

## Resolution

**Decision: do not wire it. The help and the docs stop claiming the loop runs,
and the gap is stated on the page an operator reads.** Wiring needs a product
decision the owner should make, and this ticket's own instruction is to say so
rather than pick silently.

**Why wiring needs the owner.** A bounded loop needs a `Reviewer` to produce
each pass's findings. jjfx has none: `jjfx review` sends a prompt and produces
no findings of its own, and the only reviewer jjfx ever had - a configured AI
reviewer (`[review] reviewer`, the CodeRabbit adapter in
`src/reviewer_source.rs`) - was **deliberately cut** by ADR 0016 ("review-bot
ingestion are cut"; "agent-deep means subtraction") and removed in #122. Wiring
`jjfx review --fg` to `ReviewLoop` therefore means choosing what produces
findings, and every candidate is a product decision rather than a refactor:

- re-add a configured reviewer (an external command or adapter) - reverses the
  ADR 0016 cut;
- run the Worker's agent as the reviewer - needs a findings protocol the agent
  runtime does not have (a Run's structured output is not captured today); or
- treat the pull request's own state (failing checks, verdict, conflicts) as the
  reviewer - keeps ADR 0016's cut, but changes what `review --fg` means and makes
  jjfx the reviewer the ADR says it is not.

This is exactly the boundary the second drive's map asked to settle ("jjfx is
not a reviewer" - or it needs to own the loop after all), and ADR 0016 has
already ruled on it: the review **bound** lives in jjfx, the reviewer does not.
Reopening that is the owner's call, so this ticket does not make it.

**What changed instead.** The false claim is gone, and the gap is stated where
an operator sees it:

- `jjfx help review`'s `--fg` no longer says "Run the full bounded review loop in
  the foreground"; it now says what the flag does (wait for the review Run). The
  page carries a note stating that `review` sends one prompt and runs no
  reviewer or bounded loop, that jjfx has no reviewer of its own (ADR 0016 cut
  review-bot ingestion), and that no command drives `ReviewLoop`, whose only
  caller is the harness seam.
- `docs/review-loop.md` opens by stating that no command drives the loop and
  that the bound applies when the harness drives it; the bound and replay
  sections say the same.
- `docs/configuration.md`'s `[review]` section says the cap applies to a driven
  loop, not to `jjfx review`, and the stale "what a configured reviewer does"
  paragraph - a leftover from the cut reviewer - is removed.
- `docs/work-queue.md`, `README.md` and `src/config.rs`'s doc comments stop
  implying that a command writes a review step.

`tests/help.rs` pins the page: it must not claim the bounded loop, must name
`ReviewLoop`, and must state that the loop's only caller is the harness.

**The loop itself is untouched.** `ReviewLoop`, `RecordedReviewLoop`, the
decision log and the queue wiring are real, tested and ready; this ticket only
stops the operator surface from lying about them. The follow-up, if the owner
rules that jjfx should own the loop, is to choose a reviewer and call
`RecordedReviewLoop::run` from `review_command`.

**Acceptance criteria.** AC 1 is met through its second branch. AC 2 is
conditional ("If it is wired") and so does not apply.

Verified by `mise run check` (changelog validation + fmt + clippy + build +
test), green. Changelog: `momo fix`.
