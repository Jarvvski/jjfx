# Persist Worker Pool state per pool, under a repository-wide lock

Status: accepted

## Context

ADR 0012 made a Repository hold several named Worker Pools. The persisted state
was built for exactly one: a pool document at `.jj/pool.json` beside a directory
`.jj/pool/` holding the Workers, Run Records, the waiting set, two jjfx-owned
sidecar documents and six lock files. Nothing in that layout said which state was
a pool's and which was the Repository's - a question that could not arise while
there was only one pool.

## Decision

**A directory per pool, plus an index.** `.jj/pools.json` lists the pools that
exist. `.jj/pools/<name>/` holds everything pool-scoped: the pool document, its
Workers, Run Records, the waiting set, occupancy, sessions and its locks. The
principle is the split itself - **pool-scoped state lives under the pool,
repository-scoped state stays at `.jj/`** - which keeps `ws-cache` and
`undo.json` at the root, because undo is `jj op restore` and that restore is
repository-wide.

**The jjfx-owned sidecars stay files.** Occupancy is written wholesale -
recompute the set, replace it - so as one file it is one atomic replace; as
per-Worker fields it would be N read-modify-writes with no single point of
consistency, and a crash mid-reconcile would leave a half-updated set nothing
could detect.

**Locks are both, repository outermost.** Per-pool locks for pool-scoped state; a
repository-wide lock for Workspace creation and removal, `ws-cache`, undo,
`clean` and colocation. The ordering is fixed - repository lock outermost, never
pool-then-repository - or two pools provisioning Workspaces at once deadlock.

**Existing Repositories convert automatically, once, on first access**: build the
new layout, verify it, then remove the old. The Go peer that once justified
keeping the old layout readable is dead, so there is no second reader to keep
alive.

## Considered options

- **Flat, with a `pool` field on each record** - least path churn, but every
  shared file grows a pool dimension and the locks stay shared, so one pool's
  Dispatch blocks another's.
- **Folding the sidecars into the pool or Worker documents** - removes two files,
  at the cost of the single atomic write a wholesale occupancy reconcile depends
  on.
- **Read-time fallback instead of conversion** - no big-bang, but two read paths
  kept correct indefinitely, and Repositories left half-migrated.
- **One repository-wide lock** - needs no ordering rule, and throws away most of
  the reason to have lanes.

## Consequences

- Removing a pool becomes a directory operation, which is what makes *What
  happens when a Worker Pool is removed?* a tractable question.
- The index and the directories can disagree. Which is authoritative is a
  separate decision, deliberately not settled here.
- Every path in the state layer moves, and the migration is one-way - which is
  why this is an ADR rather than a ticket comment.

**Superseded in part.** The index (`.jj/pools.json`) is dropped: the `[pools]`
declaration (ADR 0014) is the source of truth and a pool directory is state, so
there is nothing for the index to disagree with. Everything else above stands.

**Corrected in part.** The Decision's enumeration lists occupancy among the
pool-scoped state under `.jj/pools/<name>/`, and that one item is wrong:
occupancy is repository-scoped. `in-use.json` sits at the root of `.jj/`,
because it is a single reconcile over every tab the terminal has open and so
spans pools, and a whole-set replace wants the one atomic write a root file
gives it. The split, the sidecar-shape argument and the lock ordering above are
unchanged - only the list of pool-scoped items is short by one.
