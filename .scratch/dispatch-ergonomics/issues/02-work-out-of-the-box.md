# Dispatch must work with no `main` bookmark and no remote

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

Two of the first three dispatches in a fresh colocated jj repository failed on environment assumptions jjfx could have detected, and neither error named the missing piece.

**It requires a bookmark literally named `main`.** Worker Workspace provisioning passes `main` to jj (`src/engine/workspace.rs:637`). A fresh `jj git init --colocate` has no bookmarks at all:

```
"outcome": "failed", "phase": "workspace",
"detail": "prepare Worker worker-103af5 on requested base: Error: Revision `main` doesn't exist"
```

There is no fallback to `@`, `trunk()`, or the repository's default branch, and the error does not say to create one.

**It requires a remote.** jjfx resolves an `owner/name` slug from `origin` (or a pool-level `gh_repo`, `src/engine/direct_dispatch.rs:643`) to build the `gh -R owner/name pr create` line in the Worker prompt. With no remote:

```
"outcome": "failed", "phase": "identity",
"detail": "cannot resolve Direct Dispatch identity: Repository identity \"\" must use owner/name format"
```

`src/engine/ticket.rs:955` produces that. Nothing says "there is no `origin`".

The remote case is worse than a clean failure: the slug can resolve to a *wrong* repository from a local path, and Workers then dutifully run `gh -R <wrong/repo> pr create` against a real GitHub repository that is not the one they are working in.

## Solution

Make the out-of-the-box path work, and make the failures name their fix.

- **Trunk:** fall back sensibly when `main` does not exist - `@` or `trunk()` - or create it, or make the base revision a pool setting with a documented default. Do not hard-code a bookmark name with no fallback.
- **Remote:** a repository with no remote should still be able to dispatch. Either make the identity optional (a Worker can be told to commit and push without a PR) or detect the absence and say so. Do not fail with a format error on an empty string.
- **Errors:** name the missing thing and the command that fixes it, e.g. `No bookmark named 'main'; create one with: jj bookmark create main -r @` and `No origin remote; add one with: jj git remote add origin <url> (or set the pool's gh_repo)`.

## Acceptance Criteria

- [ ] A fresh colocated jj repo with no bookmarks can dispatch without a hand-made `main` bookmark - or fails with a message naming the fix and the command.
- [ ] A repository with no remote can dispatch - or fails with a message naming the missing remote and the command that adds it.
- [ ] No failure is a bare format or validation complaint about an empty value.
- [ ] Tests cover the no-trunk and no-remote paths.
- [ ] `mise run check` is green.

## References

- `src/engine/workspace.rs:637` (the hard-coded `main`)
- `src/engine/direct_dispatch.rs:643` (`repository_identity`)
- `src/engine/ticket.rs:955` (the identity format error)
- `src/config.rs` (pool settings)

## Comments

- 2026-10-16: Filed from the `todo-md` test-drive.
