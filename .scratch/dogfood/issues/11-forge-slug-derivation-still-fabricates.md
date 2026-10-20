# The forge's own slug derivation still fabricates one from a non-GitHub origin

Status: resolved
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

- [x] `derive_repo_slug` refuses, or returns no slug for, an `origin` that is
      not a GitHub remote - reusing the named message the family already
      introduced rather than inventing another.
- [x] Its two callers behave sensibly on that refusal: in particular the forge's
      Pull Request step must not turn it into a fabricated `gh -R` lookup.
- [x] A legitimate GitHub `origin` is unaffected.
- [x] Say in the Resolution whether `derive_repo_slug`/`slug_from_url` and
      `workspace.rs`'s `remote_slug`/`fabricated_slug` should be **one**
      derivation rather than two that agree by hand - and if they should, say
      what stopped you or do it.

## References

- `.scratch/dogfood/issues/03-*` (the write), `10-*` (the persisted read)
- `src/jj.rs` (`derive_repo_slug`, `slug_from_url`)
- `src/engine/workspace.rs` (`remote_slug`, `fabricated_slug`)
- Found while closing `dogfood/10` (#131)

## Resolution Tue 20 Oct 2026

`derive_repo_slug` now **refuses** - it does not return no slug for - an `origin`
that is present but not a GitHub remote. `Ok(None)` still means only "no `origin`
remote at all", which every caller already reads as "no forge here, nothing to
do"; a present-but-non-GitHub remote returns an error carrying the family's named
message (`origin is not a GitHub remote: <remote>; point `origin` at GitHub (or
set the pool's `gh_repo`)`), built by a new `jj::not_github_remote` that the
engine's `RepositoryIdentityError::NotGitHub` now also formats through, so the
family still has one message and not a fourth.

Refuse rather than return `None` because the two are not interchangeable at the
callers. `Ok(None)` is the "no forge" signal, and the forge reads it as *the Pull
Request step does not apply* and finishes the run as a **clean success** - so a
GitLab or filesystem `origin` would silently skip opening a Pull Request and
report success, which is worse than the failure the family was filed for. The
error instead lands in the forge's existing `Some(Err)` arm: the step is skipped
with `pr: origin is not a GitHub remote: <remote>; ...` as its reason and the run
is retained as not-clean, so the operator is told. It also keeps the read honest:
`SystemWorkReads::pull_requests` returns `Err`, which `snapshot` treats as an
*unknown* Pull Request state, rather than the confident empty list `Ok(None)`
would produce (a pushed commit would then look like it needs no forge).

Both callers were checked:

- `src/forge.rs` - `SystemJj::origin_slug` already maps the error to `Err`, which
  the pipeline turns into a skipped Pull Request step naming the reason; a new
  unit test drives it with a GitHub adapter that panics on every call, so a
  fabricated `gh -R` lookup would fail the test rather than pass silently.
- `src/work.rs` - unchanged code, but the behaviour is now pinned: a new test
  drives the production read port against a real repository whose `origin` is a
  local path and asserts the named refusal.

A legitimate GitHub `origin` is unaffected: `derive_repo_slug` returns the slug
as before (pinned by a unit test that sets `origin` to the scp-like GitHub form),
and the engine's persisted-slug path still resolves.

**The two derivations are now one.** `jj.rs::slug_from_url` is deleted;
`jj.rs::remote_slug`/`split_remote` - moved from `src/engine/workspace.rs` - is
the single GitHub-remote derivation, called by both `derive_repo_slug` and the
engine's `origin_slug`/`refuse_a_fabricated_slug`, with its unit test moved
beside it. The engine keeps `fabricated_slug`, but only as the recogniser for the
value a legacy Pool may still hold (nothing derives an identity from it), so
there is one derivation and one legacy recogniser rather than two derivations
that had to agree by hand. Not unified: the *remotes read* (jj's
`git remote list`) still has a copy in each layer - `jj::read_at_repo` and the
engine's `git_remotes` - but that is a read of the same jj state, not a
derivation, and the engine needs the raw remote (not a slug) to recognise a
fabricated one; collapsing it would drag `RepositoryIdentityError::Remotes`
across the seam for no correctness gain.

Verified with `derive_repo_slug_refuses_a_non_github_origin_by_name`,
`remote_slug_reads_a_github_remote_and_refuses_anything_else`,
`a_non_github_origin_is_reported_rather_than_fabricated_into_a_gh_lookup` and
`pull_requests_refuses_a_non_github_origin_by_name`, plus the unchanged
`fabricated_slug` recogniser test. `mise run check` is green.
