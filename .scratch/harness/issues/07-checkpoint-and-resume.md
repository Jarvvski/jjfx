# An interruption must not orphan in-flight work

Status: resolved
Type: task
Blocked by: none

## Problem

33 sessions were interrupted in five days, clustered in the review loop. The
drivers were killed after roughly five hours each, and **their in-flight
children died with them** - work that had been paid for and was never collected.

A workflow that is deterministic to run has to survive its own interruption:
what was in flight is recorded, and it can be resumed rather than lost or
silently re-done.

## Acceptance Criteria

- [x] Interrupting a run leaves a durable record of what was in flight, not just
      a terminal status.
- [x] In-flight work can be resumed without redoing completed steps.
- [x] Resumption is explicit - an operator asks for it - rather than automatic
      retry of something that may have side effects.
- [x] A test kills a run mid-flight and asserts the record survives and resumes.

## References

- `.scratch/harness/issues/06-durable-queue.md`
- `docs/run-records.md`, `src/engine/state.rs`

## Resolution

`06` had already built the durable record this needs - `InFlightRecord` written
*before* a Run starts, the `claimed`/`in_flight`/`done`/`failed`/`abandoned`
lifecycle, and a step identity that makes every transition idempotent. That was
verified rather than assumed: a step claimed and begun is read back from disk by
a fresh `Repository` with the Worker, Workspace, branch, runtime and PID intact,
and settling a step leaves the in-flight record on the item, so an interrupted
step is never *only* a terminal status. No path was found that left a terminal
status alone, so nothing there needed changing.

What was missing was the resumption itself, and one gap in the queue that would
have blocked it: `claim` refuses a terminal step, so a step settled `failed` or
`abandoned` - which is what reconciliation writes for a Run whose process is
gone - could never have been run again. `WorkQueue::resume` closes that: it
re-opens the record to `queued`, clears the attempt's claim and end, and carries
the state it was resumed from and why into `detail`, so the new Run's claim and
settlement apply to the step. `QueueItemState::is_resumable` is the one rule for
what may be re-opened: not `done` (the step completed, and a resumption must not
redo it) and not `in_flight` (reconciliation has just asserted that Run is live,
so re-running it would be the automatic retry of something with side effects
that a resumption must not be).

**The surface is a command: `jjfx resume [<TICKET>...] [--fg|--bg]
[--pool NAME] [--force] [--json]`.** A command rather than a `--resume` flag on
`dispatch`, because the two answer different questions: `dispatch` accepts and
runs work, discovering it from a source, while `resume` recovers work jjfx has
already accepted and did not finish, reading it from the queue - and a Ticket
with no recorded step is reported rather than dispatched. It reconciles first,
so a Run that ended is settled and a Run that is still live is recognised before
anything is re-opened, and then re-dispatches each re-opened step on the same
seam a Dispatch uses, so the capacity governor, the spend limits and the
occupancy exclusions all apply. It is explicit because an operator asks for it:
nothing re-runs on its own.

Verified by `tests/work_queue_resume.rs`: a stand-in Run is started in its own
process group, recorded on the Worker and in the queue's in-flight record
exactly as a Dispatch records it, and then **killed where it stands**; a fresh
`Repository` reads the record back from disk (Worker, Workspace, branch, PID),
`jjfx resume` re-runs that step to `done`, and the step that had already
completed is left with its original `finished_at` and no Run Record of its own.
The same file covers a Run that is genuinely still live (a real process is left
running and the resumption must leave it alone) and a resumption with nothing
unfinished. The kill is deterministic: the process is alive by construction
rather than raced, and the only process jjfx did not launch is the stand-in
itself. `mise run check` passes.
