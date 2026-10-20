# A re-dispatch that succeeds leaves the durable queue reporting the failure

Status: ready-for-agent
Type: bug
Blocked by: none

## Problem

The durable queue (`harness/06`) is meant to answer "what happened to this?". A
step is keyed by identity (`dispatch:<ticket>`), so a second Dispatch of the same
Ticket is the *same* step. When the first attempt settles `failed` and a later
Dispatch of the same Ticket succeeds, the queue item is **never updated** - it
still reads `failed`, and the failure detail is still the old one.

Observed in one pool's `queue.json` after `sift--01-bucket-counts` was first
failed by the anchor bug and then dispatched successfully:

    {"step":"dispatch:sift--01-bucket-counts","state":"failed",
     "detail":"cannot anchor Worker worker-1be12c's opencode session: ..."}

while the run record and the Ticket say the opposite:

    $ cat .jj/pools/default/runs.jsonl
    {"ticket":"sift--01-bucket-counts","conclusion":"succeeded","check":"passed", ...}
    $ jjfx ticket list
    sift--01-bucket-counts   resolved   done

`jjfx status --json`'s `queue` therefore contradicts both the Run record and the
Ticket for that step.

**The consequence is worse than a stale read: `resume` re-runs landed work.**
`resume` re-opens every step that is not `done`/`in_flight`, and the stale
`failed` step looks unfinished:

    $ jjfx resume --json
    [{"ticket":"sift--01-bucket-counts","outcome":"dispatched","worker":"worker-1be12c", ...},
     {"ticket":"sift--02-grep-filter","outcome":"dispatched","worker":"worker-4af033", ...},
     {"ticket":"sift--03-top-messages","outcome":"skipped","detail":"already done"}, ...]

Tickets 01 and 02 were already `resolved`, their branches pushed and their PRs
open; `resume` re-dispatched them anyway. (The drive interrupted those two Runs to
avoid duplicate commits, which is why the queue now also holds two extra `failed`
runs for 01/02.)

## Acceptance Criteria

- [ ] A successful Dispatch of a Ticket whose step is `failed` settles that step
      `done` (or records the successful attempt), so the queue reflects the
      latest outcome rather than the first failure.
- [ ] `resume` does not re-open a step whose work has since completed - either
      because the queue no longer reads `failed`, or because it consults the Run
      record / the Ticket's status before re-opening.
- [ ] A test drives a failed step, re-dispatches the same Ticket to success, and
      asserts the queue reads `done` and `resume` leaves it alone.

## References

- `.scratch/harness/issues/06-durable-queue.md`,
  `.scratch/harness/issues/07-checkpoint-and-resume.md`
- `src/engine/queue.rs`, `src/engine/state.rs` (`QueueItemState::is_resumable`)

## Triage (2026-10-20)

Triaged from `needs-triage` to `ready-for-agent`. The evidence is a **live
reproduction during the second drive**, not a code reading: the drive hit this
while working a real backlog through jjfx, and the report
(`.scratch/dogfood-2/report.md`) records what happened around it.

Not independently re-verified - the drive reproduced it live against the queue.

The acceptance criteria are unchanged - they were already specific and
testable, which is why this needed triage rather than a rewrite.
