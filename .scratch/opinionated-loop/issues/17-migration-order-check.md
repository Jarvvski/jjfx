# Flag migration-order inversions across open workspaces

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

Parallel Workers each edit their own jj workspace, and nothing compares what they
are about to land. For an ordered-migration project (Flyway, Alembic, Rails, or
any `V<n>__name` sequence) that is a merge hazard no per-PR check can see: two
workspaces can each add a migration, each be individually correct, green and at
zero open threads, and still be **wrong as a set**.

The concrete failure, from the October 2026 batch: two changes added migrations
`V189` and `V190` in separate workspaces. Merged in the wrong order, a
lower-numbered migration lands *after* a higher-numbered one. Flyway runs with
`outOfOrder` unset, so boot validation fails - **CI never sees it**, because CI
applies the whole set in version order on a fresh database, but staging and
production do, on the next deploy. It was caught by hand, by a human holding the
merge order in their head, and only because they had been burned by the same
shape earlier the same day.

This is the second of two cross-PR hazards the batch produced. The first is a
compile dependency (ticket 18). Both are properties of the **set** of open
changes, not of any one change, so no review of a single PR can raise them.

## Solution

A read-only check over the open workspaces' migrations, and a way to see it.

jjfx already holds everything this needs: every workspace **and its path**
(`src/jj.rs:94-124` -> `WorkspaceEntry.path`, `src/engine/workspace.rs:102-123`)
and every local bookmark (`graph.rs:127`, `view.local_bookmarks()`). No agent, no
new process, and no PR layer is required.

- Enumerate each workspace's working copy for migration files matching a
  configurable pattern (default `**/V*__*.sql`, with the pattern in config so
  non-Flyway projects are not forced into a Flyway shape).
- Parse the version prefix from each file name and flag any file whose version is
  lower than one already present **above** it in the same ordering space - the
  inversion - naming both files and both workspaces.
- Surface it where the owner will see it: an Attention/conflict-lane entry, and a
  `jjfx` verb (e.g. `jjfx check migrations`) so an agent can run it before
  asking for a merge.
- Report the ordering space explicitly. The default is "the whole set of open
  workspaces against trunk"; a project whose migrations are per-module needs the
  comparison scoped, and guessing that wrong produces false positives.

Verification-first: confirm the failure mode against a real `V*__*.sql` project
before building, and confirm that `jj`'s view of a workspace's files is enough to
read a *staged but unpushed* migration (the working copy is the right unit - not
the bookmark's committed tree, which may lag).

## Commits

1. Read and parse migrations from every open workspace's working copy, behind a
   configurable pattern.
2. Detect inversions within the ordering space and produce a typed report naming
   both files and both workspaces.
3. Surface it as a check verb and in the Attention/conflict lane.
4. Tests (an inversion is found; a clean set is not flagged; two workspaces with
   the same number are flagged as ambiguous, not silently ordered); docs; changelog.

## Acceptance Criteria

- [ ] Two workspaces adding `V189` and `V190` in either arrangement produce a
      named inversion report before either is merged.
- [ ] A set with no inversions reports clean.
- [ ] Two migrations with the **same** version are reported as ambiguous.
- [ ] The pattern is configurable and a project with no matching files is a
      no-op.
- [ ] `mise run check` is green.

## Out of Scope

- Reordering or rewriting migrations (the check reports; the owner decides).
- Non-ordered migrations (timestamps are monotonic, so they cannot invert).
- The compile dependency hazard - that is ticket 18.

## References

- `src/jj.rs:94-124` (`workspaces()`), `src/engine/workspace.rs:102-123`
  (`WorkspaceEntry`)
- `graph.rs:127` (`view.local_bookmarks()`), `src/trunk.rs`
- Flyway `outOfOrder` and validation: https://documentation.red-gate.com/flyway
- `CONTEXT.md`, ADR-0007 (repo state reads)

## Comments

- 2026-10-15: Filed from the strategy review. The closest of the three new
  tickets to jjfx's existing data - it is a pure read over workspaces jjfx
  already enumerates, so it is the cheapest way to close a cross-PR hazard.
