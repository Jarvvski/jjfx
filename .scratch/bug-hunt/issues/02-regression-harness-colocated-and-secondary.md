# Regression tests: colocated "pushed" and the secondary-workspace default root

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

Two integration seams are wrong and untested. This ticket only builds the red
tests; the fixes are tickets 06 and 11.

### D3 - git-exported bookmarks count as "pushed"

`work.rs:420-433` builds a revset on `remote_bookmarks()` and claims it
"excludes the colocated `git` remote". It does not: in a colocated repo `jj git
export` copies every local bookmark into the `git` remote view, and jj-lib's
`remote_bookmarks()` matches all remotes. An unpushed local bookmark therefore
lands in the chain and the row shows `WorkState::Pushed`. `graph.rs:138-145`
already excludes `REMOTE_NAME_FOR_LOCAL_GIT_REPO`; `work.rs` does not.

### I5 - default workspace pinned to the current workspace

`store.rs:149-162` does `default.path = Some(repo_root.to_path_buf())` where
`repo_root` is the raw `repo::discover` result (the *current* workspace root),
ignoring `Repository::open(...).root()`. Launched from a secondary workspace,
the `default` row points at that secondary and the real default disappears;
`watch::watch_repo` then watches the wrong `.jj`.

## Fix

Add integration tests (no production change in this ticket):

1. In a colocated repo, create an unpushed local bookmark and assert the
   workspace's work state is `Dirty`/`ReadyToForge`, **not** `Pushed`.
2. Launch `Store::load` from inside a non-default workspace and assert the
   `default` entry resolves to the default workspace root.

## Commits

1. Add the colocated unpushed-bookmark test to `tests/repository.rs` (or a new
   `tests/work.rs`), expected to fail today.
2. Add the secondary-workspace default-root test, expected to fail today.

## Acceptance Criteria

- [ ] Both tests exist and fail against `main`, each naming the wrong value.
- [ ] The tests encode the domain expectation from `CONTEXT.md` (a bookmark is
      only `Pushed` when it is on a remote; the default workspace is the
      repository's stable home).

## Out of Scope

- The fixes (tickets 06, 11).
