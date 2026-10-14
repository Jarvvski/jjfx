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

## Amended: jj is the source of truth for Workspace existence and paths

The decision above rests on jj not recording a Workspace's filesystem path, so
the cache had to be read back to learn one. That is no longer true: jj keeps a
path per named Workspace in its workspace store, and both `jj workspace root
--name <name>` and `jj workspace list -T 'name ++ "\t" ++ root'` report it. jjfx
therefore takes the Workspace set *and* each path from jj, and reads the cache
for neither. A name jj does not report is not a Workspace, so a stale entry
cannot resurrect one; a path jj does not record is not known, so jjfx reports it
as unknown rather than guessing at the naming convention, which is now only the
rule for where a *new* Workspace is created.

The cache is still written for the shell tooling that reads it, but it is
write-only as far as jjfx is concerned: a mirror in the strict sense. The naming
convention never resolved a path again, which is what let a Workspace created
outside it (`JJ_WS_DIR`, a script) be recorded at the wrong directory and then
reported as missing there.

Status: accepted - Workspace existence and paths read from jj; the cache is a
write-only mirror
