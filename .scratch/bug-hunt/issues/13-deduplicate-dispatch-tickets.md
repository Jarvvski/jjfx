# Deduplicate Ticket ids before reserving capacity

Status: resolved
Type: task
Blocked by: none

## Problem

Duplicate Ticket ids were never deduplicated, so one Ticket could be dispatched
twice:

- `ready_tickets` discovery pushed every parsed Ticket, so a provider returning
  the same Ready Ticket twice produced two Tickets.
- `reserve_batch_locked` selects a *distinct* Worker per input with no id dedup,
  so a batch `["ENG-1", "ENG-1"]` reserved two Workers and launched two Runs for
  one Ticket, with both outcomes reporting `ENG-1`.

## Fix

Dedup at both boundaries:

- **Discovery** (`ticket.rs`): `ready_tickets` keeps the first occurrence of a
  parsed `TicketId` and reports each repeat as a `"duplicate Ticket"` diagnostic,
  so the caller never receives two Tickets for one id.
- **Batch** (`state.rs`): `reserve_batch_locked` parses each input's Ticket id and
  rejects the whole batch with a typed `StateError` on the first repeat. A
  duplicate is a caller bug, and rejecting the batch outright is the non-silent
  behaviour the ticket asks for; no new `ReservationsOutcome` variant was needed,
  which keeps the churn (and the exhaustive matches) small.

Internal callers are unaffected: orchestration claims one Ticket at a time and
`group.ready()` is already keyed by unique ids.

## The "raw vs parsed" note in the original ticket

The ticket also flagged `dependency_graph`, where `child_counts` keys on the raw
id but `candidates` on the parsed `TicketId`. That collision is **not
reachable**: `TicketId::parse` does not normalise (`validate_component` only
validates and stores the raw string), so two raw ids that parse equal are
byte-identical, and `child_counts` already rejects byte-identical duplicates
before they reach `candidates`. Left as-is rather than adding unreachable code.

## Commits

1. `Reject duplicate Tickets at discovery and batch reservation` - both
   boundaries, two tests, and a CHANGELOG `(fix)`.

## Acceptance Criteria

- [x] A duplicate Ticket id produces exactly one Reservation and one Run.
- [x] The duplicate is reported, not silently dropped.
- [x] `mise run check` is green.

## Out of Scope

- Changing the CLI's accepted input shape.
- `validate_component` accepting ids with trailing `.`/`/` (a filename-shape
  concern, tracked with the workspace-name validation work in ticket 22).
