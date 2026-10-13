# Distinguish a failed `gh pr list` from "no Pull Requests"

Status: resolved
Type: bug
Blocked by: none

## Problem

`prs::list` returned an empty `Vec` on any failure (no `gh`, no auth, no
network), and `WorkReads::pull_requests` wrapped that in `unwrap_or_default`.
`work::snapshot` then classified every Workspace as if the repository had no Pull
Requests at all.

The visible harm: a Workspace whose Pull Request has already merged showed as
`Pushed` (still needs forging) whenever `gh` was unavailable.

## Decision

The ticket listed three options. **Option 1, narrowed**, was chosen:

- A failed read is carried through as a failure (`Result`, not an empty list).
- Only the classification that actually *depends* on Pull Request knowledge
  changes: a `Pushed` overlay becomes `Unknown` when the read failed, because
  the commit may already have an open or merged Pull Request.
- `Dirty` and `Clean` are unaffected - they are decided without Pull Requests,
  so a failed read must not throw them away.

This avoids option 1's full noise (the whole list turning `Unknown` when `gh` is
down) while never claiming a state the read cannot support. Option 2's
"single diagnostic" was rejected because it would need a new channel through
`Work`/`WorkState` for a condition the narrowed option already renders honestly.

The `--limit 100` cap is now named (`LIST_LIMIT`) and documented: a repository
with more than 100 Pull Requests reports only the newest, and the read that
decides one Workspace's state (`prs::find`) is scoped to its own branch and
unaffected. Pagination was not added because nothing needs the full history.

## Commits

1. `Report a failed Pull Request read instead of "no Pull Requests"` - the
   `Result` boundary, the narrowed `Unknown` rule, three tests, and a CHANGELOG
   `(fix)`.

## Acceptance Criteria

- [x] A failed `gh pr list` is distinguishable from an empty result at the
      `WorkReads` boundary (`Result`, not `unwrap_or_default`).
- [x] A failed read does not silently claim "no Pull Requests"; the behaviour is
      asserted by tests.
- [x] `prs::list` truncation past 100 is documented.
- [x] `mise run check` is green.

## Out of Scope

- The Forge submission path, which already used `prs::find` (a `Result`).
- Paginating the list read.
