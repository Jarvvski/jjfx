# Sweep the singular "the Pool" from the codebase

Type: task
Status: resolved
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

- [x] No bare "the Pool" remains in comments, error strings or docs; each says which pool.
- [x] `CONTEXT.md` and the ADRs read consistently with plural pools.
- [x] No new noun is introduced for the collection.
- [x] No behaviour changes - this is a wording sweep, and the test suite is unchanged.
- [x] `mise run check` is green.

## Resolution

Every bare "the Pool" in code comments, doc comments, error strings and docs now
says which pool: "the Worker Pool", the pool's own name, or the Repository's
Worker Pools. `CONTEXT.md` already read plurally and needed no edit;
`docs/configuration.md`, `docs/run-records.md` and `docs/undo.md` did.

Three user-facing strings were reworded - the two destroy failure strings and
`logs`' follow-mode stop message, whose assertion in `tests/logs.rs` moved with
it. That assertion is the only test change. `Worker Pool` is not renamed and no
collection noun was added.

ADR 0011's occupancy note was stale in three ways: it asserted the lapsed
compatibility constraint as live, named the pre-`wsg`-expunge marker path
`.jj/pool/in-use.json`, and kept a sentence about a dead compatibility peer. It
now cites ADR 0013's atomic-replace reason, uses `.jj/in-use.json`, and states
that the marker is repository-scoped. Its Dispatch Eligibility wording was
aligned with `CONTEXT.md`, as was `src/engine/eligibility.rs`.

Left alone deliberately: `CHANGELOG.md` (a dated historical record),
`.scratch/**` (issue records), ADR 0012's quotation of the phrase itself, and
other singular `Pool` attributives - `Pool capacity`, the `compatible Pool
lock`, the TUI keybinding labels - which are not "the Pool" and whose sweeping
would have turned a wording pass into a refactor.

## References

- `CONTEXT.md` (the `Worker Pool` entry), `docs/adr/0011-*`, `docs/adr/0012-named-worker-pools.md`
- `.scratch/multi-pool/issues/02-what-is-a-pool-now.md`
