# Fence untrusted Ticket content in dispatch prompts

Status: resolved
Type: task
Blocked by: none

## Problem

Ticket title, description, id, and blocker titles were concatenated verbatim
into the Worker prompt and system prompt. Validation only trimmed and checked
non-empty (`TicketTitle::parse`), so a ticket containing newlines or instructions
("ignore the above...") could hijack the Run. The same strings interpolate into
shell-looking command text, and `RepositoryIdentity::parse` accepted any
`owner/name` shape without a character restriction, so a slug such as
`owner/repo;rm` reached the `gh -R <repo> ...` string.

## Fix

- A new `untrusted_block` helper renders tracker text as a labelled,
  delimited data block: it strips control characters (keeping `\n`/`\t`), caps
  the length, and removes the closing delimiter so the content cannot forge the
  fence. Applied to the Ticket title, the inlined description, and the stacked
  blocker description.
- `RepositoryIdentity::parse` now restricts each component to GitHub's
  `[A-Za-z0-9._-]`, so the slug is safe to interpolate into the `gh` command.

## Commits

1. `Fence untrusted Ticket content in Dispatch prompts` - the helper, the three
   call sites, the Repository slug restriction, tests, and a CHANGELOG `(fix)`.

## Acceptance Criteria

- [x] Ticket text is clearly delimited as data in both prompts.
- [x] Control characters are stripped and the length is capped.
- [x] A hostile Ticket cannot forge the fence terminator.
- [x] The interpolated Repository slug cannot contain shell syntax.
- [x] `mise run check` is green.

## Out of Scope

- The `assignee` value (from `jj config user.email`) is operator-controlled, not
  Ticket-controlled; it is not fenced.
