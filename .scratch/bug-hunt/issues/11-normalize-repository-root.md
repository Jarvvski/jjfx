# Normalise the repository root once at discovery

Status: ready-for-agent
Type: task
Blocked by: 02

## Problem

`repo::discover` (`repo.rs:9`) returns the nearest `.jj`, i.e. the *current*
workspace root. `Store::load` (`store.rs:149-162`) then pins the `default`
workspace to that raw path instead of `Repository::open(...).root()`, which
correctly normalises a secondary workspace to the default root
(`engine/mod.rs:256`). The same raw path feeds `watch::watch_repo`
(`watch.rs:21`) and the work poller (`lib.rs:347-379`).

Launched from a secondary workspace: the `default` row points at the secondary,
the real default disappears, the watcher watches the wrong `.jj`, and
cache-only workspaces never get a `Work` snapshot (permanent `Unknown` / `Idle`).

## Fix

Resolve the repository root **once** through `Repository::open(...).root()` and
thread that value everywhere - `Store`, `watch_repo`, the work poller,
`Runtime`, and `Forge` - instead of each re-deriving it from the raw discovery
result. While here, stop `Store::reload` resetting `colocate` to `true`
(`store.rs:170`).

## Commits

1. Introduce one normalised root at discovery; thread it through the frontends.
2. Make the poller build names from the Store projection, not `jj` alone.
3. Stop `reload` resetting `colocate`.
4. The secondary-workspace test from ticket 02 passes.
5. CHANGELOG entry `(fix)`.

## Acceptance Criteria

- [ ] Launched from a secondary workspace, `default` resolves to the default
      root and the watcher watches the shared `.jj`.
- [ ] A cache-only workspace still receives work state.
- [ ] `store.colocate()` survives a reload.
- [ ] The regression test from ticket 02 passes.
- [ ] `mise run check` is green.

## Out of Scope

- The Worker Run cwd (ticket 05) unless ticket 01 shows a shared cause.
