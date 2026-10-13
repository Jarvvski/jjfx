# Distinguish a failed `gh pr list` from "no Pull Requests"

Status: ready-for-agent
Type: bug
Blocked by: none

## Problem

`prs::list` returns an empty `Vec` on any failure (no `gh`, no auth, no
network), and `WorkReads::pull_requests` wraps that in `unwrap_or_default`
(`src/work.rs:142`). `work::snapshot` then classifies every Workspace as if the
repository had no Pull Requests at all.

The visible harm: a Workspace whose Pull Request has already merged shows as
`Pushed` (still needs forging) whenever `gh` is unavailable, instead of `Merged`.
It is a wrong-but-safe direction - re-forging is idempotent - which is why this
is separate from, and lower than, ticket 10's I1/I3/I4.

There is a second, smaller issue in the same read: `list` uses `--limit 100`
with no pagination, so a repository with more than 100 Pull Requests silently
truncates.

## Decision Needed (implementer)

What should a Workspace render when the Pull Request read failed but its chain
read succeeded? Options:

1. `WorkState::Unknown` - honest, but the whole list turns Unknown whenever `gh`
   is down, which is noisy.
2. Keep the current degradation but surface a single diagnostic ("Pull Request
   state unavailable: <error>") so the operator knows the list is incomplete.
3. Keep the current per-Workspace degradation and add the diagnostic only when a
   workspace would otherwise have been `Pushed`/`Merged`.

Option 2 is the smallest honest change and is the recommended default.

## Acceptance Criteria

- [ ] A failed `gh pr list` is distinguishable from an empty result at the
      `WorkReads` boundary (`Result`/`Option`, not `unwrap_or_default`).
- [ ] A failed read does not silently claim "no Pull Requests"; the chosen
      option's behaviour is asserted by a test.
- [ ] `prs::list` truncation past 100 is either paginated or documented.
- [ ] `mise run check` is green.

## Out of Scope

- The Forge submission path, which already uses `prs::find` (a `Result`).
