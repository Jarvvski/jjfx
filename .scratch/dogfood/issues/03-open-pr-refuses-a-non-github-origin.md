# `open-pr` must refuse a non-GitHub `origin` instead of guessing a slug

Status: ready-for-agent
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

- [ ] A remote that is not a GitHub remote is detected, and the failure says so
      (`origin is not a GitHub remote`) rather than naming a fabricated slug.
- [ ] The fabricated slug is not written into pool state.
- [ ] The missing-`origin` message from `dispatch-ergonomics/02` still holds.
- [ ] A test drives a local-path `origin` and asserts the named refusal.

## References

- `src/engine/workspace.rs:1605` (`remote_slug`), `:1567` (`repository_identity`)
- `.scratch/dispatch-ergonomics/issues/02-work-out-of-the-box.md`
