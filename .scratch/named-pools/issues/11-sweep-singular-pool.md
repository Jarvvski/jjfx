# Sweep the singular "the Pool" from the codebase

Type: task
Status: ready-for-agent
Blocked by: 01, 02, 03, 04, 05, 06, 07, 08, 09, 10

## Problem

ADR 0012 gave the collection no new noun: a Repository holds *its Worker Pools*, so the bare definite "the Pool" is no longer meaningful on its own and must always say which pool it means. Every such phrase in code comments, error strings and documentation is now ambiguous.

This is **deliberately last**. It is mechanical and it touches nearly everything - the state layer, the CLI, `CONTEXT.md` and ADR 0011 - so running it after the structural work keeps it from conflicting with every ticket above it.

## Scope

- Every bare "the Pool" in code comments, error strings and user-facing output becomes explicit: "the Worker Pool", "the `<name>` pool", or "the Repository's Worker Pools".
- `CONTEXT.md`'s `Worker Pool` entry and anything in `docs/` that assumes one pool.
- ADR 0011's occupancy note, which says the marker is "kept out of the compatibility documents" - the *constraint* has already been annotated as lapsed, so check whether the wording still reads correctly.
- **Do not rename `Worker Pool` itself**, and do not introduce a collection noun - that is the decision, not an oversight.

## Acceptance Criteria

- [ ] No bare "the Pool" remains in comments, error strings or docs; each says which pool.
- [ ] `CONTEXT.md` and the ADRs read consistently with plural pools.
- [ ] No new noun is introduced for the collection.
- [ ] No behaviour changes - this is a wording sweep, and the test suite is unchanged.
- [ ] `mise run check` is green.

## References

- `CONTEXT.md` (the `Worker Pool` entry), `docs/adr/0011-*`, `docs/adr/0012-named-worker-pools.md`
- `.scratch/multi-pool/issues/02-what-is-a-pool-now.md`
