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
  Ticket is `dispatch:<ticket>`, so the same Ticket offered twice is the same
  step and a repeat is detectable rather than a silent second Run.
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

## Durability, and what it costs

The queue is written atomically and read from disk, never recomputed from live
Worker state, so it outlives the process that wrote it.

Writes at the Dispatch seam are best effort, exactly as the waiting set is. A Run
that has already launched must not be misreported, or have its Worker released,
because a state write failed. The failure mode is a stale queue on an unwritable
disk, not a lost Run: every transition is keyed by step identity and is
idempotent, so a later reconciliation settles the same step.
