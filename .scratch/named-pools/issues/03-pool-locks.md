# Locks: per-pool inside a repository-wide lock

Type: task
Status: resolved
Blocked by: 02

## Problem

Today there is one pool and one set of locks, so lock *scope* has never had to be a decision. With several pools it is, and the wrong answer is a race between lanes rather than within one.

**ADR 0013** settles it:

- **Per-pool locks** for pool-scoped state - dispatch, workers, runs, waiting, sessions - living inside the pool's directory.
- **A repository-wide lock** for repository-scoped operations: Workspace creation and removal (jj's workspace store is repository-level), `ws-cache`, undo, `clean`, colocation.
- **The ordering is fixed: repository lock outermost, never pool-then-repository.** This is the load-bearing part - without it, two pools provisioning Workspaces at once deadlock.

## Scope

- Give each pool its own lock files inside `.jj/pools/<name>/`, replacing the single `.jj/pool/.dispatch.lock` and friends.
- Introduce (or promote) the repository-wide lock and route repository-scoped mutations through it.
- **Enforce the ordering in code, not by convention.** A single helper that acquires repository-then-pool makes the rule checkable; a bare pair of `flock` calls does not.
- Keep the lock *behaviour* - the existing lock-free reads that are safe because writes are atomic replacements - unchanged.

## Acceptance Criteria

- [x] Each pool's state is guarded by locks inside its own directory.
- [x] Repository-scoped operations take the repository-wide lock.
- [x] The acquisition order is repository-then-pool everywhere, with a test that would catch the reverse.
- [x] Two pools provisioning Workspaces concurrently neither deadlock nor corrupt each other's state.
- [x] `mise run check` is green.

## References

- `src/engine/state.rs` (the `*_LOCK` constants and the repository facade)
- `docs/adr/0013-persist-pool-state-per-pool.md`
- `.scratch/multi-pool/issues/03-pool-state-layout-and-migration.md` (the locking decision)
