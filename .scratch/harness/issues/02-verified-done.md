# A Ticket is done only when a check actually ran and passed

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

`resolved` is written when a Run completes with a successful conclusion - the
agent's own terminal report. Nothing verifies that the work is correct, or even
finished. In a real drive, all four Tickets read `resolved` and none of the work
was on trunk.

This is the trust boundary the whole loop rests on, and the research is
unusually clear about it. Anthropic's own example: "a flight-booking agent might
say 'Your flight has been booked'... the outcome is whether a reservation exists
in the environment's SQL database." Huang et al. show LLMs cannot self-correct
without external feedback, and sometimes get worse. The Replit post-mortem is
the cautionary case: production and dev shared a database, so *no prompt could
have prevented it* - the fix was architectural.

Ticket 18 already built most of the mechanism: a repo-defined `[checks]` gate
that runs between weld and push. What is missing is the wiring - the Ticket
status still reflects the run, not the check.

## Acceptance Criteria

- [ ] A Ticket reaches its done status only when the repository's check ran
      against the change and passed; a Run that finishes with no check having
      run does not read as done.
- [ ] A failed check leaves the Ticket demonstrably not-done, naming what failed.
- [ ] A repository with no check configured behaves predictably and says so
      rather than silently claiming verification.
- [ ] `dogfood/06` is closed by this, pointing at the decision.

## References

- `.scratch/dogfood/issues/06-resolved-means-run-finished-not-landed.md`
- `.scratch/opinionated-loop/issues/18-forge-build-check.md`
- `src/checks.rs`, `src/engine/pool.rs` (reconcile), `src/engine/local_tickets.rs`
