# A named Ticket that is not ready must say why

Type: task
Status: ready-for-agent
Blocked by: none

## Question

*The ready set must say why it is empty* named two cases in its Solution: an empty ready set, **"or a Ticket the user named is not ready"**. Its acceptance criteria covered only the first, and the resolution implemented only the first — so `jjfx dispatch <TICKET>` still consults no readiness at all. Name a Ticket whose `Status:` is not the ready status, or one blocked by an unresolved sibling, and jjfx dispatches it without a word.

That is the same silence the empty-ready-set fix removed from `--all`, one path over. The `--all` path now computes exactly the right reason (`LocalTickets::not_ready_reason`) and renders it through `ReadyTickets::diagnostics`; the named path never asks.

Decide what the named path should do, then do it:

- **Report and proceed** — the user named the Ticket deliberately, so naming it *is* the override; print why it is not ready and dispatch it anyway.
- **Report and refuse** — readiness gates every path, and naming a not-ready Ticket is an error that states the reason.

The first reads truer to today's behaviour and to "report why"; the second is more consistent, and would make an explicit override flag the way to bypass it. Whichever is chosen, the reason should come from the same `not_ready_reason` seam, so the two paths cannot drift apart.

## Answer

<!-- recorded on resolution -->
