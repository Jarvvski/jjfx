# What is a Pool now that there are several?

Type: grilling
Status: ready-for-human
Blocked by: none

## Question

`CONTEXT.md` defines **Worker Pool** as "the Repository-scoped collection of reusable Workers and their execution capacity" — singular, one per Repository. **Repository** is in turn defined as "the container for its Workspaces and Trunk and **the scope of its Worker Pool**". This effort replaces that: several pools per Repository, each owning a size, an Agent Runtime Profile, a spend cap, and its own Ticket source.

Three things have to be decided, in this order, and they are the owner's to decide:

1. **What is a pool now?** The same term redefined, or a new one? It is no longer the whole capacity of a Repository; it is one lane of it. And what is a Worker's relationship to it — does a Worker *belong* to exactly one pool for its life, or is it assigned to one per Dispatch? That distinction decides the persisted shape (see the layout ticket).
2. **What is the collection called?** The Repository now holds a set of pools, and `Repository`'s own definition names the pool as its scope. "Pool Set"? "Lanes"? Something else? The glossary needs a term.
3. **What breaks in the glossary?** **Dispatch Eligibility** is defined as "a **Pool** member whose Worker Status is idle and that is not Occupied" — pool-relative, now ambiguous. Work through every entry that references the Pool and say which survive unchanged.

This is a HITL session: it resolves in conversation, one question at a time. Vocabulary lands in `CONTEXT.md` as it is settled. If the outcome is hard to reverse, surprising to a future reader, and a genuine trade-off, it earns an ADR superseding the relevant part of 0009.

## Answer

<!-- recorded on resolution -->
