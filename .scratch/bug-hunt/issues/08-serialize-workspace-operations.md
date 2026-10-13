# Serialise every Workspace operation on the Worker lock

Status: ready-for-agent
Type: task
Blocked by: 03

## Problem

`lock_worker_operation` serialises Workspace preparation and Reset restoration,
but only some callers take it:

- **Take it:** `direct_dispatch.rs:374` -> `workspace.rs:612`
  (`prepare_for_dispatch`); `worker_actions.rs:453-481` (`reset`).
- **Skip it:** `worker_actions.rs:579-643` (`follow_up` / `send` / `review`) and
  `:370-387` (`rebase`).

`reset` marks the Worker idle *before* a detached restoration thread runs
`jj restore` + `jj new main` (`reset_run` -> `clear_run`, `pool.rs:443-482`). A
follow-up Run dispatched in that window edits the working copy mid-restore,
corrupting it or losing work. The lock is applied inconsistently across callers,
which is the actual defect.

## Fix

Route every Workspace-mutating path through one seam that takes
`lock_worker_operation` for the duration of the operation - `send`, `review`,
`rebase`, and Mount included - or make Reset keep the Worker non-idle until
restoration completes. Prefer the single seam so the rule is enforced once.

## Commits

1. Introduce the uniform Workspace-operations seam (or hold the Worker
   non-idle until restoration finishes).
2. Move `send`/`review`/`rebase` onto it.
3. Tests: the Reset/`send` serialisation test from ticket 03 passes.
4. CHANGELOG entry `(fix)`.

## Decision Document

- Chosen shape: one `WorkspaceOps` seam that owns the lock, vs. keeping the lock
  in each caller. Record why.

## Acceptance Criteria

- [ ] No Workspace-mutating path runs without `lock_worker_operation` (or the
      equivalent non-idle guarantee).
- [ ] A Reset followed immediately by a follow-up cannot interleave
      working-copy mutations.
- [ ] The regression test from ticket 03 passes.
- [ ] `mise run check` is green.

## Out of Scope

- Replacing the file-lock implementation (ticket 25).
