# Deduplicate Ticket ids before reserving capacity

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

Duplicate Ticket ids are never deduplicated, so one Ticket can be dispatched
twice:

- `reserve_batch_locked` (`state.rs:1601-1652`) selects a distinct Worker per
  input with no id dedup.
- `direct_dispatch.rs:209-225` and discovery (`ticket.rs:699-719`) do not dedup
  either.

A batch `["ENG-1","ENG-1"]`, or a provider returning the same Ready Ticket
twice, reserves two Workers and launches two Runs for one Ticket; both outcomes
report `ENG-1`.

A related raw-vs-parsed gap: `ticket.rs:732-788` keys `child_counts` on the raw
id but `candidates` on the parsed `TicketId`, so two raw ids that parse equal
but differ as strings (whitespace is permitted by `validate_component`,
`state.rs:172-184`) are neither flagged nor merged - the second silently
overwrites the first and drops its diagnostics.

## Fix

Dedup by parsed `TicketId` at both the discovery boundary and the batch
boundary, and reject duplicates with a diagnostic rather than silently
double-dispatching.

## Commits

1. Dedup + diagnostic at discovery (`ticket.rs`).
2. Dedup + diagnostic at reserve/batch (`state.rs`, `direct_dispatch.rs`).
3. Tests: duplicate ids produce one reservation and one diagnostic.
4. CHANGELOG entry `(fix)`.

## Acceptance Criteria

- [ ] A duplicate Ticket id produces exactly one Reservation and one Run.
- [ ] The duplicate is reported, not silently dropped.
- [ ] `mise run check` is green.

## Out of Scope

- Changing the CLI's accepted input shape.
