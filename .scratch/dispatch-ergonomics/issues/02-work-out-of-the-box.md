# Dispatch must work with no `main` bookmark and no remote

Status: resolved
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

- [x] A fresh colocated jj repo with no bookmarks can dispatch without a hand-made `main` bookmark - or fails with a message naming the fix and the command.
- [x] A repository with no remote can dispatch - or fails with a message naming the missing remote and the command that adds it.
- [x] No failure is a bare format or validation complaint about an empty value.
- [x] Tests cover the no-trunk and no-remote paths.
- [x] `mise run check` is green.

## References

- `src/engine/workspace.rs:637` (the hard-coded `main`)
- `src/engine/direct_dispatch.rs:643` (`repository_identity`)
- `src/engine/ticket.rs:955` (the identity format error)
- `src/config.rs` (pool settings)

## Comments

- 2026-10-16: Filed from the `todo-md` test-drive.
- 2026-10-16: Resolved. **Trunk:** `prepare_for_dispatch` bases a Worker Workspace on `main`, then `master`, then the default Workspace's working-copy commit, so a fresh repository with no bookmarks dispatches without a hand-made `main`. **Remote:** a repository with no `origin` (or no usable slug) now fails with `no origin remote ...; add one with jj git remote add origin <url> (or set the pool's gh_repo)` instead of the bare `owner/name` format error. Chose to refuse loudly rather than make identity optional: a Worker with no remote cannot push or open a PR, so its delivery contract cannot be honoured, and a named failure is more honest than a Worker that silently cannot deliver.
- 2026-10-16: Not addressed here: a local-path `origin` (e.g. `/path/jarvis/todo-md.git`) still resolves to a slug (`jarvis/todo-md`) that may name an unrelated GitHub repository. Refusing it needs a GitHub-host policy decision (GitHub Enterprise, `gh_repo` as the override), so it wants an owner ruling rather than an agent guess.

## Re-opened Mon 19 Oct 2026

Reconciled against the merged code at `6d6f3b689e0e`. Two criteria do **not** hold as written, so this ticket is not done and is back to ready-for-agent.

The no-trunk half shipped and is tested. The no-remote half was fixed on the **Direct Dispatch** path (`src/engine/direct_dispatch.rs:697` filters an empty slug and names the fix) but **not** on the single-Ticket orchestration path: `src/engine/orchestration.rs:1107` still calls `RepositoryIdentity::parse(pool.gh_repo())` with no empty guard, so a repository with no `origin` whose pool has an empty `gh_repo` still fails with the bare `Repository identity "" must use owner/name format` - the exact failure these two criteria set out to remove.

To close it: give that call site the same fallback or named error the Direct Dispatch path got, and cover the orchestration path with a no-origin test.

## Resolution Mon 19 Oct 2026

Closed the orchestration half by sharing one resolver rather than copying the message. `src/engine/workspace.rs` gains `repository_identity(root, configured)` and `remote_slug`, plus a `RepositoryIdentityError` that names the missing `origin` and the command that adds it. Both paths now call it: `src/engine/direct_dispatch.rs` delegates (its private `remote_slug` is gone), and `src/engine/orchestration.rs` `prepare` no longer calls `RepositoryIdentity::parse(pool.gh_repo())` bare - it resolves from the pool's `gh_repo` then `origin`, and records the resolved slug on the group. `src/engine/pool.rs` reuses the shared `remote_slug`, so the slug derivation exists once.

Verified with two orchestration tests: `prepare_names_the_missing_remote_instead_of_an_empty_slug_format_error` (no `origin`, empty `gh_repo`, real colocated repo - the failure names `jj git remote add origin` instead of the empty-slug format error) and `prepare_derives_identity_from_origin_when_the_pool_gh_repo_is_empty` (an `origin` with an empty `gh_repo` now prepares a group carrying `owner/repo`). `mise run check` is green.
