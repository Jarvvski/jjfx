# Multi-pool

<!-- wayfinder:map -->

## Destination

A locked design for running **several Worker Pools in one Repository** — each pool owning its size, its Agent Runtime Profile, its spend cap under a global ceiling, and its own Ticket source — precise enough that implementation tickets can be cut from it without further decisions. CLI and persisted state only; the TUI is out of scope.

## Notes

- **Domain**: jjfx. This effort changes the Workspace Dispatch domain, so start from `CONTEXT.md` (the glossary) and `docs/adr/0009-workspace-dispatch-domain.md`, which defines **Worker Pool** as *Repository-scoped* and singular. Read both before resolving any ticket — several decisions below are really glossary decisions.
- **Tracker**: local markdown — this file plus `.scratch/multi-pool/issues/`. See `docs/agents/issue-tracker.md` (Wayfinding operations).
- **Type**: a `Type:` line on each ticket. Grilling tickets are **HITL** — they resolve only in live conversation with the owner, one question at a time; an agent that answers its own questions has broken the ticket, not resolved it.
- **Status**: `ready-for-human` on a grilling ticket (it needs the owner), `ready-for-agent` on AFK work, `claimed` while a session holds it, `resolved` when done.
- **Standing preference — additive, not a redesign.** `jjfx pool <N>` should keep working, and `status --json` should keep its flat `workers[]` gaining a `pool` field rather than nesting. That JSON shape changed *hours* before this effort began; breaking it again would be the worst kind of churn. See the additive-or-redesign ticket.
- **Skills**: `/grilling` and `/domain-modeling` for the decision tickets. Vocabulary lands in `CONTEXT.md` as it is settled; a decision that is hard to reverse, surprising to a future reader, and a genuine trade-off earns an ADR.
- **Provenance**: the idea came from the owner while reviewing the `todo-md` test-drive, and the sharpest fork (where pools are declared) is the same wall that test-drive hit wanting `[tickets] source = "local"` per repository.

## Decisions so far

<!-- the index — one line per closed ticket: enough to judge relevance, then zoom the link for the detail the ticket holds -->

## Not yet specified

- **Borrowing capacity between pools.** Can an idle "features" Worker take a "bugs" Ticket when the bugs pool is saturated? Unclear until the domain model lands, and it changes what a pool's "size" means.
- **What removing a pool does with its Workers' branches.** Ties to the known gap where a Reset Worker's earlier branches become unreachable from `open-pr` / `rebase` / `absorb`.
- **Whether pools need their own Lift / Tidy / Reset semantics.** Probably not — those are per-Workspace — but a plural Pool model may force a distinction.

## Out of scope

- **The TUI.** The primary interface is deferred to its own effort once the model settles. The cost of deferring it, stated plainly: we may lock a model the TUI cannot wear comfortably, and `CONTEXT.md` is an attention-first UI model.
- **Multi-repository pools.** *Repository* is defined as "the scope of its Worker Pool"; a pool spanning repositories is a different effort.
