# A named Ticket that is not ready must say why

Type: task
Status: resolved
Blocked by: none

## Question

*The ready set must say why it is empty* named two cases in its Solution: an empty ready set, **"or a Ticket the user named is not ready"**. Its acceptance criteria covered only the first, and the resolution implemented only the first — so `jjfx dispatch <TICKET>` still consults no readiness at all. Name a Ticket whose `Status:` is not the ready status, or one blocked by an unresolved sibling, and jjfx dispatches it without a word.

That is the same silence the empty-ready-set fix removed from `--all`, one path over. The `--all` path now computes exactly the right reason (`LocalTickets::not_ready_reason`) and renders it through `ReadyTickets::diagnostics`; the named path never asks.

Decide what the named path should do, then do it:

- **Report and proceed** — the user named the Ticket deliberately, so naming it *is* the override; print why it is not ready and dispatch it anyway.
- **Report and refuse** — readiness gates every path, and naming a not-ready Ticket is an error that states the reason.

The first reads truer to today's behaviour and to "report why"; the second is more consistent, and would make an explicit override flag the way to bypass it. Whichever is chosen, the reason should come from the same `not_ready_reason` seam, so the two paths cannot drift apart.

## Answer

Resolved 2026-10-16. **Report and proceed.** Naming a Ticket by hand is a
deliberate act, so naming it is the override: `jjfx dispatch <TICKET>` reports
why the named Ticket is not ready and dispatches it anyway. Refusing would turn
today's working Dispatch into an error for a case the user chose explicitly, and
would need an override flag to restore the behaviour this ticket was filed to
make audible.

The reason comes from the same seam `--all` uses: `cli::dispatch_command` now
calls `TicketDiscovery::ready_tickets` for the named path (through
`report_named_ticket_readiness`) and reports each named Ticket found in
`ReadyTickets::diagnostics`, which the local source fills from
`LocalTickets::not_ready_reason`. So a blocked Ticket names its blocker and that
blocker's status, and a wrong-status Ticket names both statuses, in the same
wording `--all` prints - the two paths cannot drift apart. The check runs before
the Orchestrator handoff, so a single named Ticket - which falls back to a direct
Dispatch - reports too; it is skipped for `--all` (which reports its own) and for
ad-hoc input (which has no source).

The report is a stderr diagnostic printed alongside the dispatch result, for both
prose and `--json`, mirroring how `--all` prints diagnostics when the ready set is
non-empty: the Ticket was dispatched, not skipped, so the JSON array stays one
object per Ticket - its real outcome - and no `Skipped` entry is fabricated, which
would also mask ticket 04's all-failed exit code. It is gated to
`source = "local"`: the Linear query filters by label and status, so a named
Ticket it does not return carries no reason to report, and asking would add a
query and a failure mode to every named Dispatch for nothing.

Tests in `tests/dispatch_outcomes.rs` drive the real binary for a blocked named
Ticket (orchestration path, prose), a wrong-status named Ticket
(`--no-orchestrate`, `--json`), and a ready named Ticket (no false positive);
all pass `--force` so they do not depend on the ambient terminal. `mise run
check` is green.
