# The work queue is a sidecar, not a jj artefact

Status: accepted

## Context

jjfx records what it has queued, claimed, run and settled nowhere durable. The
waiting set describes one Dispatch and is rewritten by the next; a Dispatch Group
tracks one parent Ticket's Sub-issues. An interrupted Run therefore leaves no
record of what was in flight, and a repeated step cannot be told from a new one.

The record has to survive the process that wrote it and answer "what happened to
this?" after the fact. ADR 0016 left where it should live to the tickets; this
settles it.

Two properties of jj bear on the answer: every jj *read* mutates the
repository's operation log, and jj records no tool or process attribution, so
concurrent writers are indistinguishable in that log.

## Decision

**The queue is a sidecar document**, `.jj/pools/<name>/queue.json`, beside
`runs.jsonl` and `waiting.json`, written with the state layer's atomic replace
under its own sidecar lock.

jj is the substrate jjfx orchestrates, not its bookkeeping. A queue in jj - as a
bookmark, a commit, or a file in a commit - would make every reader a writer of
the operation log, which is the log an operator reads to understand what happened
to their repository; and because jj attributes an operation to no tool or process,
a concurrent jjfx could not be told from a hand-run `jj` command, or from another
jjfx. The queue's whole purpose is to answer what happened, and a jj-native
artefact would answer it ambiguously while polluting the log it lives in.

The sidecar also gets the concurrency story right for free: the state layer
already serialises writers with a per-document sidecar lock and commits with a
single atomic rename, so a queue update is all-or-nothing and two processes cannot
interleave a partial write. A jj-native artefact would need its own coordination
on top, against a substrate that does not offer compare-and-swap.

## Considered options

- **A bookmark pointing at a queue commit.** The queue would be visible in `jj
  log` and diffable. Rejected: reading it mutates the log, every update is a
  commit in the operator's history, and concurrent updates are an unmergeable
  conflict rather than a serialised write.
- **A file in a jj commit.** Same log pollution, plus the queue would be pushed
  and merged like repository content, when it is machine state local to one
  checkout.
- **A repository-scoped sidecar at `.jj/queue.json`.** Simpler path, but
  everything else about a Dispatch - Workers, Run Records, the waiting set - is
  pool-scoped (ADR 0013), and a Repository holds several pools (ADR 0012). One
  queue at the root would make a pool's work indistinguishable from another's.
- **Keep inferring from live Worker state.** No new state to keep correct, and
  exactly the behaviour that lost 33 sessions' in-flight children: once the Worker
  is gone, the work is gone with it.

## Consequences

- The queue is jjfx-owned state, so it is covered by the same compatibility rules
  as the other sidecars and lives under the pool's own directory.
- Reading the queue is free: `status` does not add an operation to the log.
- The queue is not a jj artefact, so it is not pushed, merged, or visible in
  `jj log`. An operator reads it through `jjfx status --json` or `jq`.
- Writes at the Dispatch seam are best effort, as the waiting set is: a Run that
  has already launched must not be misreported because a state write failed.
  Idempotent, step-keyed transitions make a later reconciliation sufficient.
