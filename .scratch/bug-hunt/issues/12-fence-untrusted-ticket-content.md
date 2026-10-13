# Fence untrusted Ticket content in dispatch prompts

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

Ticket title, description, id, and blocker titles are concatenated verbatim into
the Worker prompt and system prompt (`dispatch_prompt.rs:149-193`, `:136-145`).
Validation only trims and checks non-empty (`ticket.rs:1026-1040`). A ticket
containing newlines or instructions ("ignore the above...") can hijack the Run.
The same strings interpolate into shell-looking command text
(`dispatch_prompt.rs:130-135`, `:178-182`), and neither `RepositoryIdentity` nor
`TicketId` rejects quotes (`ticket.rs:618-636`, `state.rs:172-184`).

## Fix

- Delimit and label untrusted content (clear begin/end fences naming it as
  Ticket-supplied data).
- Strip control characters and cap length.
- Escape interpolated repository/prefix/assignee values used in command strings,
  or render them as data rather than as a command template.

## Commits

1. Add a fencing/escaping helper for untrusted prompt content.
2. Apply it to title, description, id, and blocker titles.
3. Tests: a malicious ticket cannot break the fence or inject a command.
4. CHANGELOG entry `(fix)`.

## Acceptance Criteria

- [ ] Ticket text is clearly delimited as data in both prompts.
- [ ] Control characters are stripped and length is capped.
- [ ] Interpolated repository/prefix values cannot inject shell syntax.
- [ ] `mise run check` is green.

## Out of Scope

- Sanitising anything outside the prompt (Linear itself).
