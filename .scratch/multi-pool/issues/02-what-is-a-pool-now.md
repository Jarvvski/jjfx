# What is a Pool now that there are several?

Type: grilling
Status: resolved
Blocked by: none

## Question

`CONTEXT.md` defines **Worker Pool** as "the Repository-scoped collection of reusable Workers and their execution capacity" — singular, one per Repository. **Repository** is in turn defined as "the container for its Workspaces and Trunk and **the scope of its Worker Pool**". This effort replaces that: several pools per Repository, each owning a size, an Agent Runtime Profile, a spend cap, and its own Ticket source.

Three things have to be decided, in this order, and they are the owner's to decide:

1. **What is a pool now?** The same term redefined, or a new one? It is no longer the whole capacity of a Repository; it is one lane of it. And what is a Worker's relationship to it — does a Worker *belong* to exactly one pool for its life, or is it assigned to one per Dispatch? That distinction decides the persisted shape (see the layout ticket).
2. **What is the collection called?** The Repository now holds a set of pools, and `Repository`'s own definition names the pool as its scope. "Pool Set"? "Lanes"? Something else? The glossary needs a term.
3. **What breaks in the glossary?** **Dispatch Eligibility** is defined as "a **Pool** member whose Worker Status is idle and that is not Occupied" — pool-relative, now ambiguous. Work through every entry that references the Pool and say which survive unchanged.

This is a HITL session: it resolves in conversation, one question at a time. Vocabulary lands in `CONTEXT.md` as it is settled. If the outcome is hard to reverse, surprising to a future reader, and a genuine trade-off, it earns an ADR superseding the relevant part of 0009.

## Answer

Resolved in conversation with the owner, 2026-10-16. Three decisions:

1. **A Worker belongs to exactly one Worker Pool for its life** — membership, not assignment. The pool's `workers` list is that membership, exactly as it is today, and "size" is the member count. Assignment per Dispatch was rejected because a Worker is backed by exactly one Worker Workspace and that Workspace accumulates one Change chain: a Worker that moved between lanes would carry one lane's work into another. An explicit Reset-backed move between pools is *not* in this design.
2. **`Worker Pool` stays the term for the unit, and the collection gets no new noun.** A Repository holds *its Worker Pools*. "Pool Set" was rejected as a term almost nothing operates on; "Lane" was rejected as a large vocabulary break for a naming win. The cost is real and recorded: the bare definite "the Pool" is no longer meaningful on its own.
3. **Glossary sweep applied** — `Repository`, `Worker Pool`, `Worker`, `Dispatch Eligibility` and `Dismiss` redefined; `Occupied`, `Worker Status`, `Reset`, `Reservation`, `Run`, `Trunk` and `Change chain` survive unchanged. **`Pool Workspace` was a synonym slip** for `Worker Workspace` and is collapsed.

Landed as `CONTEXT.md` plus **ADR 0012**, which supersedes the Repository-scoped Pool definition in ADR 0009 (annotated to point at it). The ADR records the rejected alternatives and the consequence that every bare "the Pool" in code, error strings and docs must be made explicit.

Fog graduated: *What happens when a Worker Pool is removed?* and *Do the maintenance verbs need a pool scope?*. *Borrowing capacity between pools* is decided against — membership is the model — and moved to Out of scope rather than left as fog.
