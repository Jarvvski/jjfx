# `open-pr` still uses a stale fabricated `gh_repo` from existing pool state

Status: resolved
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

- [x] The verbs that resolve the persisted `gh_repo` refuse a slug that could not
      have come from a GitHub remote, with the named message `dogfood/03`
      introduced, **before** contacting `gh`.
- [x] A pool carrying a legacy fabricated `gh_repo` produces that message rather
      than a GraphQL lookup.
- [x] A legitimate `gh_repo` is unaffected.

## Out of Scope

- Migrating or repairing legacy pool state. Refusing at the read is enough;
  rewriting history is a separate decision.

## References

- `.scratch/dogfood/issues/03-open-pr-refuses-a-non-github-origin.md`
- `src/engine/workspace.rs` (`origin_slug`)
- `src/engine/worker_actions.rs` (`repository_slug`, `open_pull_request`)

## Resolution Tue 20 Oct 2026

The read is closed where the write was, and with the same named refusal.
`src/engine/workspace.rs` gains `refuse_a_fabricated_slug(root, configured)`:
it reads the Repository's `origin` and, when that remote is not a GitHub one,
refuses a persisted slug equal to the value the pre-`dogfood/03` derivation
mined from it - the last two path segments for a filesystem path, or the path
after the last `:` for the scp-like shape (`fabricated_slug`, kept only to
recognise a legacy value; `remote_slug` is still the one derivation). The
refusal is `RepositoryIdentityError::NotGitHub`, so the message is the one
`dogfood/03` introduced - `origin is not a GitHub remote: <remote>; point
origin at GitHub (or set the pool's gh_repo)` - and no second message exists.
It is deliberately best-effort: an `origin` that is absent or unreadable is not
a fabrication, so a verb that only needed the persisted slug never fails on the
read. `origin_remote` is factored out of `origin_slug` so both share one remotes
read.

Every verb that resolves the persisted slug was found and covered through that
one check:

- `open-pr` (`WorkerActions::open_pull_request`) and `review`
  (`pull_request_for`), both through the new `WorkerActions::persisted_gh_repo`
  - the choke point the old free `repository_slug` was, now fallible.
- `send` and `send_any`, which interpolate the slug into the agent's system
  prompt: the same read, and the empty-slug behaviour is unchanged.
- Direct Dispatch (`DirectDispatch::repository_identity`) and orchestration
  (`prepare`), both already funnelling through
  `workspace::repository_identity`, which now calls the check in its
  `configured` branch.

The forge pipeline is not covered and needs none: `crate::jj::derive_repo_slug`
reads the live `origin`, never the persisted `gh_repo`.

Verified: a unit test pins `fabricated_slug` to the dogfood drive's exact value
(`personal/dogfood-site-origin`); `prepare_refuses_a_legacy_fabricated_gh_repo_by_name`
and `open_pull_request_refuses_a_legacy_fabricated_gh_repo_before_calling_gh`
drive a real local-path `origin` and a Pool carrying that slug, and the latter
proves the refusal precedes the network by asserting a PATH-instrumented `gh`
was never invoked; `prepare_keeps_a_configured_gh_repo_that_is_not_the_fabricated_slug`
and `open_pull_request_still_reaches_gh_with_a_configured_gh_repo` keep the same
non-GitHub `origin` and show a real `owner/repo` is untouched - it still reaches
`gh`. `mise run check` is green.
