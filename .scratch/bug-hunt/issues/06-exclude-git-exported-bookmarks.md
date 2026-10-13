# Exclude git-exported bookmarks from the "pushed" work state

Status: ready-for-agent
Type: task
Blocked by: 02

## Problem

In a colocated repository (jjfx's default, ADR-0010) `jj git export` copies
every local bookmark into the `git` remote view, and jj-lib's
`remote_bookmarks()` matches all remotes. `work.rs:420-433` assumes the
colocated `git` remote is excluded, so an unpushed local bookmark makes the
chain commit look `pushed` and the row reports `WorkState::Pushed`. A user can
believe a branch is published when it is not.

`graph.rs:138-145` already excludes `REMOTE_NAME_FOR_LOCAL_GIT_REPO` for the
immutable set; the same exclusion is missing from the work query.

## Fix

Restrict the work revset to real remotes (exclude the local `git` remote), the
same way `graph.rs` does. Confirm against `jj 0.46.0` that the fixed revset
returns only `origin` (and other true remotes).

## Commits

1. Fix the revset in `work.rs`; add the colocated unpushed-bookmark test from
   ticket 02 as the permanent regression.
2. CHANGELOG entry `(fix)`.

## Acceptance Criteria

- [ ] An unpushed local bookmark in a colocated repo reports `Dirty`
      (or `ReadyToForge`), never `Pushed`.
- [ ] A genuinely pushed bookmark still reports `Pushed`.
- [ ] The regression test from ticket 02 passes.
- [ ] `mise run check` is green.

## Out of Scope

- Changing the `WorkState` model or Attention mapping.
