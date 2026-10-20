# The forge's own slug derivation still fabricates one from a non-GitHub origin

Status: ready-for-agent
Type: bug
Blocked by: none

## Problem

`dogfood/03` stopped a non-GitHub `origin` being fabricated into an `owner/name`
slug and **persisted**; `dogfood/10` closed the **read** of that persisted value.
Both fixed `src/engine/workspace.rs`.

`src/jj.rs::derive_repo_slug` is a **second, independent derivation**, and it was
never fixed: `slug_from_url` takes the last two `/`- or `:`-separated segments
of *any* URL, with no check that the remote is GitHub at all. For a local-path
or GitLab `origin` it still returns a plausible-looking `owner/name`.

It has two callers:

- `src/forge.rs` - the Pull Request step
- `src/work.rs`

So the fabricated-slug family is not closed. The forge can still derive a slug
that could not have come from a GitHub remote and hand it to `gh`, which is the
same operator experience the family was filed for: a GraphQL error naming a
repository that never existed.

Found while closing `dogfood/10` (#131), which read the persisted value rather
than this live one.

## Acceptance Criteria

- [ ] `derive_repo_slug` refuses, or returns no slug for, an `origin` that is
      not a GitHub remote - reusing the named message the family already
      introduced rather than inventing another.
- [ ] Its two callers behave sensibly on that refusal: in particular the forge's
      Pull Request step must not turn it into a fabricated `gh -R` lookup.
- [ ] A legitimate GitHub `origin` is unaffected.
- [ ] Say in the Resolution whether `derive_repo_slug`/`slug_from_url` and
      `workspace.rs`'s `remote_slug`/`fabricated_slug` should be **one**
      derivation rather than two that agree by hand - and if they should, say
      what stopped you or do it.

## References

- `.scratch/dogfood/issues/03-*` (the write), `10-*` (the persisted read)
- `src/jj.rs` (`derive_repo_slug`, `slug_from_url`)
- `src/engine/workspace.rs` (`remote_slug`, `fabricated_slug`)
- Found while closing `dogfood/10` (#131)
