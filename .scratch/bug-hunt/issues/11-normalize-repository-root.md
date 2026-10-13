# Normalise the repository root once at discovery

Status: resolved
Type: task
Blocked by: none

## Problem

`repo::discover` returned the nearest `.jj`, i.e. the **current** workspace
root. `Store::load` then pinned `default` to that raw path instead of the
repository's default workspace root, and the same raw path fed
`watch::watch_repo`, the work poller, `Runtime`, and `RealJj`. Launched from a
secondary workspace, the `default` row pointed at the secondary, the real
default disappeared, the watcher watched the wrong `.jj`, and cache-only
workspaces never received a work snapshot.

Reproduced with the built binary:

```
$ (cd ws-secondary && jjfx --list)
default   /…/ws-secondary     # wrong before the fix
```

## Fix

Normalise at the discovery chokepoint so every caller shares one root:

- `repo::discover` now resolves the repository's default workspace root through
  `Repository::open` (which follows a secondary workspace's `.jj/repo` file to
  the shared store).
- `Store::load` normalises defensively too, so a directly-passed secondary path
  cannot shadow `default`.

## Commits

1. `Normalise the repository root at discovery` - `src/repo.rs` +
   `src/store.rs`, unit tests in both, CHANGELOG `(fix)`.

## Acceptance Criteria

- [x] Launched from a secondary workspace, `default` resolves to the repository
      root.
- [x] `Store::load` cannot be made to pin `default` to a secondary workspace.
- [x] Unit tests cover both the discovery normalisation and the store pin.
- [x] `mise run check` is green.

## Out of Scope

- `Store::reload` resetting `colocate` (folded into ticket 28).
