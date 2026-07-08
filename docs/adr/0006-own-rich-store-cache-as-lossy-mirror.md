# jjfx owns an authoritative rich store; .jj/ws-cache is a lossy mirror

jjfx keeps its own richer, authoritative store of workspace state rather than
treating `.jj/ws-cache` (a thin `name\tpath` list) as its model. The cache
physically cannot hold what jjfx tracks - two lifecycles, session associations,
event-derived state, forge history - so it is demoted to a projection: jjfx
writes through to it so the bash tools keep working, and reads it back to
reconcile workspaces created from a shell (folded into the rich store with empty
lifecycle state until events arrive). The cache is a mirror, never the source of
truth.

## Consequences

- The store lives per-repo, co-located and self-cleaning; the global JSONL event
  log (ADR 0004) remains the cross-repo substrate. A cross-repo dashboard is
  deferred as its own decision.
- Persist only what cannot be derived (labels/notes, pin order, forge history).
  The agent lifecycle is folded from the event log; the work lifecycle is queried
  live from `jj` and `gh`, so the store stays lean and cannot rot out of sync
  with reality.

Status: accepted
