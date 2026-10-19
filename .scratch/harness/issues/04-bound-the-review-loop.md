# Bound the review loop deterministically

Status: resolved
Type: task
Blocked by: none

## Problem

The review loop has no bound. One PR spent **144 reviewer sessions** across
passes up to 18 and was still iterating when its driver was killed after ~5h;
11 drain sessions account for 693 of 991 reviewers in five days.

The owner's own `ticket-batch` skill records the same lesson from the other
side: "14 triggers produced 18 runs: a third of the reviewer compute bought
nothing", and "three design iterations produced three new findings and removed
none". The instruction to stop therefore exists and does not work - because it
is prose in a prompt, and the loop is not bounded by anything the tool enforces.

The research's pattern is specific: bound the retries, require progress, and
escalate rather than spin (12-factor Factor 9 - keep a counter, limit the
attempts, escalate to a human, and note that over-doing this makes agents spin
out).

## Acceptance Criteria

- [x] A review loop has a maximum number of rounds, enforced by the tool.
- [x] Progress is required: a round that finds nothing new (or removes nothing)
      ends the loop rather than starting another.
- [x] Hitting the bound escalates - it reports the unresolved findings and stops,
      rather than silently succeeding or spinning.
- [x] A test drives a loop that would previously have run unbounded and asserts
      it terminates with the right report.

## Resolution

The bound lives in `ReviewLoop` (`src/engine/reviewer.rs`), so the tool enforces
it and no prompt has to.

- **The cap.** `max_passes` (`[review] max_passes`, default 2) remains the hard
  ceiling: the loop can never run more reviewer passes than it, whatever the
  reviewer reports.
- **Progress.** Each round now compares the pass's findings with the previous
  pass's, by content. To keep iterating a round must both surface a finding the
  previous pass had not and retire one it had. A reviewer that repeats itself
  finds nothing new; a Worker that fixes nothing retires nothing; either is a
  stall, so the loop stops rather than buying another pass. A pass that produces
  no findings at all is convergence and stops as before.
- **Escalation.** A loop that stops on the cap or on a stall has not converged,
  so it returns a `ReviewEscalation` carrying the findings it could not resolve
  (`ReviewStop::PassCap` / `ReviewStop::NoProgress`, and `needs_escalation()`).
  `jjfx review` prints them and stops; it never reports a stalled or capped loop
  as a clean pass.

The demonstrating test is
`a_reviewer_that_never_converges_terminates_on_the_progress_rule`: a reviewer
that adds a finding every pass and retires none, under a cap of 1,000,000, would
have run to the cap; it now stops after two passes with
`ReviewStop::NoProgress` and the two unresolved findings. `mise run check` is
green. Documented in `docs/review-loop.md` and `docs/configuration.md`, and
recorded with `momo fix`.

One decision: the CLI reports the escalation on stderr and exits 0, as the
ticket defines escalating as reporting and stopping; it does not turn a
non-converged review into a command failure, which would change `review`'s
contract for every caller.

## References

- `.scratch/opinionated-loop/issues/08-review-bot-ingestion.md`
- `src/engine/reviewer.rs` (`ReviewLoop`)
- `.agents/skills/ticket-batch/SKILL.md`
