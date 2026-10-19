# A durable work queue

Status: ready-for-agent
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

- [ ] Queued work is durable: it survives a restart of jjfx and is not
      recomputed from live worker state.
- [ ] Each item records what it is, when it was claimed, by what, and its
      outcome - enough to answer "what happened to this?" after the fact.
- [ ] The queue is readable as structure (`--json`) with stable field names.
- [ ] Reconciliation reads the queue rather than inferring completion from live
      state alone.

## Out of Scope

- Scheduling policy (priority, ordering) beyond a single default order.

## References

- `src/engine/state.rs` (`waiting.json`, run records)
- `src/engine/orchestration.rs` (dispatch groups)
- `docs/adr/0009-workspace-dispatch-domain.md`
