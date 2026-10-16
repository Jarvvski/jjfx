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
- **`wsg` is dead (owner ruling, 2026-10-16).** The Go project was archived 2026-08-19, the Rust compatibility binary has been removed, and the cutover evidence was waived. The compatibility promise in `.scratch/wsg-migration/PRD.md` no longer constrains the persisted state — **do not treat it as a contract**, and do not let it narrow a layout option. See [the research ticket's asset](assets/01-wsg-compat-contract.md).

## Decisions so far

<!-- the index — one line per closed ticket: enough to judge relevance, then zoom the link for the detail the ticket holds -->

- [Is the persisted Pool state a `wsg` compatibility contract?](issues/01-is-pool-state-a-wsg-contract.md) — **Ours.** The Go project is archived, the Rust compatibility binary is removed, and the cutover evidence was waived, so the persisted state is jjfx's to reshape. Two things carry forward: additive fields were *always* allowed, and the dead contract's residue (fixtures, ADR 0011's constraint, three sidecar documents) is still live — retired separately by *Retire the dead `wsg` compatibility residue*.
- [What is a Pool now that there are several?](issues/02-what-is-a-pool-now.md) — A Worker **belongs to exactly one pool for its life** (membership, not assignment); `Worker Pool` stays the unit and the collection gets **no new noun** — a Repository holds *its Worker Pools*, so the bare "the Pool" must always name which. Glossary swept; `Pool Workspace` collapsed as a synonym slip for `Worker Workspace`. Landed as **ADR 0012** plus `CONTEXT.md`, superseding the Repository-scoped definition in ADR 0009.
- [Where do pools live on disk, and how does today's pool migrate?](issues/03-pool-state-layout-and-migration.md) — **A directory per pool** (`.jj/pools/<name>/`) plus a small index (`.jj/pools.json`), split by the principle *pool-scoped under the pool, repository-scoped at the root* — so `undo.json` stays at `.jj/`, because undo is repository-wide. The jjfx-owned sidecars stay **files** (occupancy is written wholesale; one atomic replace beats N read-modify-writes). Existing Repositories convert **automatically, once, on first access** — verify, then remove. Locks are **both, repository outermost**, which is a rule that must be written down or two pools deadlock. Landed as **ADR 0013**.

## Not yet specified

<!-- see "Fog of war": in-scope fog you can't ticket yet; graduates as the frontier advances -->

Currently empty. The three items here — borrowing capacity between pools, what removing a pool does with its Workers' branches, and whether pools need their own Lift / Tidy / Reset semantics — graduated when the domain model resolved: the first was decided against (membership is the model, so it moved to Out of scope), and the other two became tickets.

## Out of scope

- **The TUI.** The primary interface is deferred to its own effort once the model settles. The cost of deferring it, stated plainly: we may lock a model the TUI cannot wear comfortably, and `CONTEXT.md` is an attention-first UI model.
- **Multi-repository pools.** A Worker Pool is held by one Repository, and its Workers are backed by that Repository's Workspaces; a pool spanning repositories is a different effort.
- **Moving or borrowing Workers between pools.** Membership is the model: a Worker belongs to one pool for its life, so an idle "features" Worker cannot take a "bugs" Ticket. A Reset-backed move is a conceivable future effort, but it is not this design — decided by *What is a Pool now that there are several?*.
