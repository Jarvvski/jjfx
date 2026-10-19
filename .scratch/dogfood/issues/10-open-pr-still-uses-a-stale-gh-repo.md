# `open-pr` still uses a stale fabricated `gh_repo` from existing pool state

Status: ready-for-agent
Type: bug
Blocked by: none

## Problem

`dogfood/03` stopped a non-GitHub `origin` being turned into a fabricated
`owner/name` slug and persisted as the pool's `gh_repo`. That closes the write.
It does not close the read: `open-pr` resolves the repository identity from the
**persisted** `gh_repo`, so a pool that already carries a fabricated slug - the
exact state the dogfood left behind - still fails with the message that sent the
operator hunting for a repository that never existed:

    Error: cannot open pull request: GraphQL: Could not resolve to a Repository
    with the name 'personal/dogfood-site-origin'. (repository)

So the dogfood's own experience is only fixed for pools created after the
change. The worker who landed `03` scoped itself to the persistence point and
said so; this is the other half.

## Acceptance Criteria

- [ ] The verbs that resolve the persisted `gh_repo` refuse a slug that could not
      have come from a GitHub remote, with the named message `dogfood/03`
      introduced, **before** contacting `gh`.
- [ ] A pool carrying a legacy fabricated `gh_repo` produces that message rather
      than a GraphQL lookup.
- [ ] A legitimate `gh_repo` is unaffected.

## Out of Scope

- Migrating or repairing legacy pool state. Refusing at the read is enough;
  rewriting history is a separate decision.

## References

- `.scratch/dogfood/issues/03-open-pr-refuses-a-non-github-origin.md`
- `src/engine/workspace.rs` (`origin_slug`)
- `src/engine/worker_actions.rs` (`repository_slug`, `open_pull_request`)
