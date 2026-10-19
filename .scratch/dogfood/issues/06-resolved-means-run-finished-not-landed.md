# A Ticket reads `resolved` when its run finishes, not when its work lands

Status: resolved
Type: design
Blocked by: none

## Problem

Reconciliation flips a Ticket to `Status: resolved` when its Worker's run
completes with `exit_code 0` - it reads the durable run record, not the state of
trunk. In a drive where every landing verb failed, all four Tickets read
`resolved`, the pool read `4 done, 0 failed`, and none of the work was on
`main`.

That is defensible as "the agent finished", but the board is the operator's only
signal, and it says the work is done. The gap is invisible until you look at
trunk - which is exactly what the drive had to do to notice.

Worth deciding: does `resolved` mean the run finished, or the work landed? If
the former, the vocabulary should say so and something else should carry
landing; if the latter, reconciliation needs a landing signal.

## Acceptance Criteria

- [x] The meaning of `resolved` is decided and written down.
- [x] The board distinguishes "the run finished" from "the change landed", or
      says plainly that it does not.
- [x] A drive where landing fails does not leave every Ticket reading as done.

## Resolution

Decided 2026-10-19: a Ticket is done only when a repo-defined check actually ran
and passed against the change - never on the agent's own report. The decision and
its evidence are recorded in `.scratch/harness/map.md`; the work is
`.scratch/harness/issues/02-verified-done.md`.

The research behind the decision: this is the most-repeated pattern across
independent primary sources (Anthropic's "the outcome is whether a reservation
exists in the database"; Huang et al. on self-correction failing without external
feedback; the Replit post-mortem, where the root cause was architectural and no
prompt could have prevented it). Ticket 18 already built the `[checks]` gate, so
this was mostly wiring the status to it.

Closed 2026-10-19 by `.scratch/harness/issues/02-verified-done.md`: the check's
outcome is now recorded on the Run Record, a local Ticket is resolved only when
its most recent Run succeeded **and** passed the check, a failed or unrun check
leaves the Ticket not-done with the reason named, and a repository with no
`[checks]` configured never silently claims verification. So the board now
distinguishes the run finishing (the Worker's `done` status) from the change
being verified (the Ticket's `resolved`), and a drive where the check does not
pass no longer leaves every Ticket reading as done.
