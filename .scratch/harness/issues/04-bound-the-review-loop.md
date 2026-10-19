# Bound the review loop deterministically

Status: ready-for-agent
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

- [ ] A review loop has a maximum number of rounds, enforced by the tool.
- [ ] Progress is required: a round that finds nothing new (or removes nothing)
      ends the loop rather than starting another.
- [ ] Hitting the bound escalates - it reports the unresolved findings and stops,
      rather than silently succeeding or spinning.
- [ ] A test drives a loop that would previously have run unbounded and asserts
      it terminates with the right report.

## References

- `.scratch/opinionated-loop/issues/08-review-bot-ingestion.md`
- `src/engine/reviewer.rs` (`ReviewLoop`)
- `.agents/skills/ticket-batch/SKILL.md`
