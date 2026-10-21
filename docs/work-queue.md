# The durable work queue

`waiting.json` is the held-back set for *one* Dispatch and is rewritten by the
next. A Dispatch Group is a progress tracker for one parent Ticket's Sub-issues.
Neither is a queue: nothing recorded what was queued, what was claimed, what was
in flight, or what was abandoned, so nothing could be resumed and nothing
replayed.

The work queue is that record. Every unit of work jjfx accepts is a **step**,
identified by what it is rather than when it ran, and each step's item is written
to disk before it is claimed and again as it ends.

## Where it lives

The queue is `.jj/pools/<name>/queue.json`, inside the pool's own state
directory, beside `runs.jsonl` and `waiting.json`. It is jjfx-owned and
deliberately kept out of the pool and Worker documents so their shape is
preserved.

It is a sidecar document rather than a jj bookmark or commit. A jj *read* mutates
the repository's operation log, so a queue held in jj would make every reader a
writer and put a queue entry in the log for every question asked of it; and jj
records no tool or process attribution, so two jjfx processes writing the queue
would be indistinguishable in that log. The state layer already gives the sidecar
what the queue needs: an atomic replace of the whole document and a sidecar lock
(`.jj/pools/<name>/.queue.lock`) that serialises concurrent writers. See
`docs/adr/0017-the-work-queue-is-a-sidecar.md`.

## The record

Each item carries:

- `step` - the step's identity, derived from what it is. A Dispatch of one
  Ticket is `dispatch:<ticket>`, and a review of one dimension of a Ticket's
  change for one pass is `review:<ticket>:<dimension>:<pass>`; the same work
  offered twice is the same step, so a repeat is detectable rather than a
  silent second Run.
- `kind`, `subject` - what sort of step it is and the human-facing subject (the
  Ticket).
- `state` - the lifecycle: `queued`, `claimed`, `in_flight`, `done`, `failed` or
  `abandoned`.
- `sequence` - the position the step took when it was queued, so the queue has one
  default order.
- `queued_at`, `claimed_at`, `claimed_by`, `started_at`, `finished_at` - when the
  step was accepted, when a Worker claimed it and which, when its Run started, and
  when it ended.
- `detail` - why the step is queued, failed or abandoned.
- `in_flight` - what a resumption needs: the Worker, its Workspace, the Ticket's
  bookmark, the Agent Runtime and model, and the session or process the Run
  launched.

`in_flight` is written before the Run starts, not after, so a jjfx killed between
the claim and the launch still knows what was in flight and where. That is the
record a resumption reads.

## What a step is

A step's identity is derived from what it is, never from when it ran, so the same
work offered twice is the same step rather than two. Two kinds exist:

- `dispatch:<ticket>` - dispatching one Ticket.
- `review:<ticket>:<dimension>:<pass>` - reviewing one dimension of one Ticket's
  change for one pass.

The review kind is the one that recurs: the same dimension is reviewed again on
a later round, and a re-pass re-runs every dimension whether or not it found
anything last time. Naming the Ticket, the dimension and the pass makes such a
repeat the *same* step, so offering it again is reported as already known - the
admission says `known` and the record's state is what it settled to - rather than
running it a second time. Two passes, or two dimensions, are distinct steps.

## Replaying a decision

A step's *decision* - what it concluded, rather than that it ran - is kept in a
second sidecar, `.jj/pools/<name>/decisions.json`, keyed by the same step
identity. For a review pass the decision is the findings it produced, so a repeat
of that step reads them back instead of asking the model to derive them again.

The store is written atomically under its own lock, exactly as the queue is, and
recording the same decision twice is a no-op - so a replay does not churn it. A
step with nothing recorded is the model's to decide, and the fresh decision is
recorded under the step's identity for the next repeat to replay.

A review loop driven by the harness writes every pass through that log. Each
pass is keyed by its step identity, so the same `(ticket, dimension, pass)`
offered again - a retry, a resumed harness, a duplicate dispatch - is served from
the log rather than reaching the reviewer, and its findings are not re-addressed:
they were addressed when the decision was first made. Replay is transparent to
the bound in [the review loop](review-loop.md), so the pass cap and the progress
rule see exactly the rounds the first drive saw.

`jjfx review` drives the same bound but keeps replay to a single invocation: its
passes are keyed and recorded in the queue, but its decisions are not written to
the durable sidecar. A review pass decides on the Pull Request's *current* review
threads, so a decision replayed by a later invocation would serve a verdict the
threads have since contradicted - a recorded "no findings" hiding a thread opened
afterwards.

An operator tells a replayed pass from a pass that ran: the step stays `done`, and
its `detail` names the replay rather than leaving it silent.

```bash
jjfx status --json | jq '.queue[] | select(.step | startswith("review:"))'
```

A pass that ran carries no such detail, so "it replayed" and "it did nothing" are
never confused - which is the failure this record exists to remove.

## The states

| State | Means |
|---|---|
| `queued` | Accepted and waiting: the governor held it back, an explicit partial Dispatch found no capacity, or a launch was released before it ran. |
| `claimed` | A Worker holds the step; its Run has not started. |
| `in_flight` | The Run has started. |
| `done` | The step's work completed. |
| `failed` | The step's work failed, or it was claimed and failed before its Run started. |
| `abandoned` | The step was given up on without running to a conclusion - its Worker left the Pool, or moved on to other work. |

A terminal state is written once: the writer that observed the end owns it, and a
later settlement of the same step is a no-op.

## Reading it

`jjfx status --json` carries the queue as a flat, pool-tagged array, so a step
that was queued, claimed or abandoned is visible as structure rather than only as
live Worker state:

```bash
jjfx status --json | jq '.queue[] | select(.state == "abandoned")'
jjfx status --json | jq '.queue[] | select(.pool == "bugs" and .state == "queued")'
```

Every field name above is stable. An entry looks like:

```json
{
  "step": "dispatch:AMBA-42",
  "kind": "dispatch",
  "subject": "AMBA-42",
  "state": "in_flight",
  "pool": "default",
  "sequence": 0,
  "queued_at": "2026-10-19T10:00:00Z",
  "claimed_at": "2026-10-19T10:00:01Z",
  "claimed_by": "worker-01",
  "started_at": "2026-10-19T10:00:02Z",
  "finished_at": null,
  "detail": null,
  "in_flight": {
    "worker": "worker-01",
    "workspace": "/code/widgets-workspaces/worker-01",
    "branch": "amba-42",
    "runtime": "opencode",
    "provider": null,
    "model": null,
    "session": "ses_abc",
    "pid": null
  }
}
```

## Reconciliation

Reconciliation reads the queue rather than inferring completion from live state
alone. `reconcile_runs` iterates the durable items after it has observed the
Workers, and resolves every claimed or in-flight step:

- a Worker still running the step's Run leaves it in flight;
- a Worker whose Run ended settles it `done` or `failed`;
- a Worker that has left the Pool, or has moved on to another Ticket, abandons it,
  naming which.

Without the queue those cases are indistinguishable from a step that never
existed, which is how an interrupted Run's work was lost. A queued step is left
alone: it is work jjfx has accepted and not yet run, and only a Dispatch moves it.

## Resumption

An interruption - jjfx killed with Runs in flight - leaves the queue holding
steps that were claimed or in flight, with the `in_flight` record naming where
each one was. `jjfx resume [<TICKET>...]` re-runs that work. Resumption is
explicit: an operator asks for it, because re-running a step may have side
effects an automatic retry must not take on.

What a resumption re-runs is the queue's own per-step state, not a second notion
of progress:

| State | A resumption |
|---|---|
| `queued` | re-offers it: the work was accepted and has not run |
| `claimed` | re-offers it: the claim was written, the Run never started |
| `in_flight` | leaves it: reconciliation has just asserted its Run is live |
| `done` | leaves it: the step completed, and a resumption must not redo it |
| `failed` | re-opens it: the work did not finish |
| `abandoned` | re-opens it: the work did not finish |

`resume` reconciles first, so a Run that ended is settled and a Run that is
still live is recognised before anything is re-opened. Re-opening clears the
attempt's claim and its end and records why the step was resumed in `detail`,
so the new Run's claim and settlement apply to this step; a step another process
re-opened first is left to it rather than dispatched twice.

A Run the queue still believes is live is not raced. To abandon one anyway,
reset its Worker first - `jjfx pool reset <worker>` ends the Run, and the
reconciliation `resume` runs then abandons the step, because the Worker no
longer holds the Ticket - and then resume. Naming Tickets narrows a resumption
to those steps; a Ticket with no recorded step is reported rather than
dispatched, because `jjfx dispatch` is the command that runs work jjfx has not
accepted.

A resumed Run is dispatched on the same seam a Dispatch uses, so the capacity
governor, the spend limits and the occupancy exclusions all apply. The Run
Records keep the interrupted attempt and the resumed one as separate entries,
so what the interruption cost and what the resumption cost are both readable.

## Durability, and what it costs

The queue is written atomically and read from disk, never recomputed from live
Worker state, so it outlives the process that wrote it.

Writes at the Dispatch seam are best effort, exactly as the waiting set is. A Run
that has already launched must not be misreported, or have its Worker released,
because a state write failed. The failure mode is a stale queue on an unwritable
disk, not a lost Run: every transition is keyed by step identity and is
idempotent, so a later reconciliation settles the same step.
