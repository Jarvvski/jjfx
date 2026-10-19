# `open-pr` must refuse a non-GitHub `origin` instead of guessing a slug

Status: resolved
Type: bug
Blocked by: none

## Problem

`remote_slug` (`src/engine/workspace.rs:1605`) derives an `owner/name` slug from
any remote URL by taking the last two path segments. For a filesystem path that
is nonsense, and nothing checks that the remote is a forge:

    origin = /Users/jarvis/Code/personal/dogfood-site-origin.git
    -> personal/dogfood-site-origin

That slug is written into pool state as `gh_repo`, and the failure surfaces far
from its cause:

    $ jjfx open-pr worker-12af32
    Error: cannot open pull request: GraphQL: Could not resolve to a Repository
    with the name 'personal/dogfood-site-origin'. (repository)

Failing is correct - there is no forge here - but the message sends the
operator hunting for a repository that was never going to exist, and the bogus
slug has already been persisted.

This is distinct from the missing-`origin` case fixed in
`dispatch-ergonomics/02`: there the slug was empty; here it is non-empty and
invented.

## Acceptance Criteria

- [x] A remote that is not a GitHub remote is detected, and the failure says so
      (`origin is not a GitHub remote`) rather than naming a fabricated slug.
- [x] The fabricated slug is not written into pool state.
- [x] The missing-`origin` message from `dispatch-ergonomics/02` still holds.
- [x] A test drives a local-path `origin` and asserts the named refusal.

## References

- `src/engine/workspace.rs:1605` (`remote_slug`), `:1567` (`repository_identity`)
- `.scratch/dispatch-ergonomics/issues/02-work-out-of-the-box.md`

## Resolution Mon 19 Oct 2026

`remote_slug` is now GitHub-aware: it splits a remote into its host and path and
returns `Some(owner/name)` only when the host is `github.com`, so a filesystem
path or another forge yields `None` instead of a slug mined from its last two
path segments. Both call sites build on it rather than adding a second path:
`repository_identity` gains a `NotGitHub` error
(`origin is not a GitHub remote: <remote>`), and Worker Pool creation goes
through a new shared `origin_slug(root)` helper, so `discover_gh_repo` refuses a
non-GitHub `origin` before `load_or_create` commits anything - the fabricated
slug is never written as the Pool's `gh_repo`.

Verified with three tests: a unit test covering the GitHub scp and URL forms and
refusing a local path, a GitLab URL and a `file://` URL;
`pool_creation_refuses_a_non_github_origin_before_persisting_a_slug` (a real
local repository as `origin`, asserting the named refusal and `Loaded::Missing`);
and `prepare_refuses_a_non_github_origin_instead_of_guessing_a_slug`
(orchestration identity). The missing-`origin` message from
`dispatch-ergonomics/02` is untouched, and its two tests still pass.
`mise run check` is green.
