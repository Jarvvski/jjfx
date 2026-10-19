# A Ticket reads `resolved` when its run finishes, not when its work lands

Status: needs-triage
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

- [ ] The meaning of `resolved` is decided and written down.
- [ ] The board distinguishes "the run finished" from "the change landed", or
      says plainly that it does not.
- [ ] A drive where landing fails does not leave every Ticket reading as done.
