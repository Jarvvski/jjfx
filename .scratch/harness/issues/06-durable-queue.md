# A durable work queue

Status: resolved
Type: task
Blocked by: none

## Problem

There is no durable queue. `waiting.json` is the held-back set for *one*
dispatch, rewritten each time; the dispatch group is a progress tracker for one
parent's sub-issues. Neither is a queue: nothing records what is queued, what
was claimed, what is in flight, or what was abandoned - so nothing can be
resumed, and nothing can be replayed.

This is the piece the "workspace + queue + state layer" claim actually needs,
and the research puts it at the centre: deterministic orchestration is the thing
that is recorded and replayed, while the agent's decisions are not.

It blocks resumption (`07`) and idempotency (`09`), because both need a durable
record of what a step was.

## Acceptance Criteria

- [x] Queued work is durable: it survives a restart of jjfx and is not
      recomputed from live worker state.
- [x] Each item records what it is, when it was claimed, by what, and its
      outcome - enough to answer "what happened to this?" after the fact.
- [x] The queue is readable as structure (`--json`) with stable field names.
- [x] Reconciliation reads the queue rather than inferring completion from live
      state alone.

## Out of Scope

- Scheduling policy (priority, ordering) beyond a single default order.

## References

- `src/engine/state.rs` (`waiting.json`, run records)
- `src/engine/orchestration.rs` (dispatch groups)
- `docs/adr/0009-workspace-dispatch-domain.md`

## Resolution

A durable, pool-scoped work queue records every unit of work jjfx accepts, in
`src/engine/queue.rs` (the `WorkQueue` facade and reconciliation) and
`src/engine/state.rs` (the wire types and the `queue.json` repository).

It is a sidecar document, `.jj/pools/<name>/queue.json`, written with the state
layer's atomic replace under its own sidecar lock, beside `runs.jsonl` and
`waiting.json`.

Each unit of work is a **Step**, identified by what it is rather than when it ran
(`dispatch:<ticket>`), so a repeat is detectable rather than a silent second Run
(what `09` needs). Each **Queue Item** records what it is (`step`, `kind`,
`subject`), its lifecycle (`state`: `queued`, `claimed`, `in_flight`, `done`,
`failed`, `abandoned`), the order it was queued in (`sequence`), when it was
queued and claimed and by what (`queued_at`, `claimed_at`, `claimed_by`), when it
started and ended (`started_at`, `finished_at`, `detail`), and what a resumption
needs (`in_flight`: Worker, Workspace, branch, runtime, provider, model, session
or pid) - written *before* the Run starts, so a jjfx killed between the claim and
the launch still knows what was in flight (what `07` needs).

Dispatch admits every requested step before it claims anything, moves a step to
`claimed` then `in_flight` as the Reservation and the launch happen, settles it
from the Run's conclusion in the same writer that appends the Run Record, and
returns a step whose launch never happened to `queued`. Reconciliation reads the
queue: `reconcile_runs` resolves every claimed or in-flight step against the
Worker it recorded - still running leaves it, a finished Run settles it, and a
Worker that has left the Pool or moved on abandons it, naming which - so work
whose Worker is gone is resolved rather than silently forgotten.

`jjfx status --json` gains a flat, pool-tagged `queue` array carrying those
fields with stable names.

**Design decision: a sidecar, not a jj artefact.** Every jj *read* mutates the
repository's operation log, and jj attributes an operation to no tool or process.
A queue held as a bookmark or a commit would therefore make every reader a writer
of the log an operator reads to understand what happened, and would make two jjfx
processes indistinguishable in it - the queue exists to answer what happened, and
a jj-native artefact would answer it ambiguously while polluting the log it lives
in. The sidecar also gets concurrency for free from the state layer's
compare-and-swap replace. Rejected: a bookmark pointing at a queue commit; a file
in a jj commit (the same log pollution, plus a machine-state file pushed and
merged like repository content); a single repository-scoped `.jj/queue.json`
(everything else about Dispatch is pool-scoped - ADR 0012, ADR 0013); and
keeping to live Worker state alone, which is the behaviour that lost the
in-flight children. Recorded in `docs/adr/0017-the-work-queue-is-a-sidecar.md`,
with the surface documented in `docs/work-queue.md`.

Writes at the Dispatch seam are best effort, as the waiting set already is: a Run
that has already launched must not be misreported, or have its Worker released,
because a state write failed. Every transition is keyed by step identity and is
idempotent, so a later reconciliation settles the same step; the failure mode is
a stale queue on an unwritable disk, not a lost Run.

Verified by `tests/work_queue.rs` (a Dispatch in its own process is re-read from
disk by a fresh one and read back through `status --json`; reconciliation
resolves, abandons and leaves-running steps from a queue a killed process left
behind), the queue's unit tests in `src/engine/queue.rs`, and the JSON shape
asserted in `src/cli.rs`. `mise run check` passes.
