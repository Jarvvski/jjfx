# Is the persisted Pool state a `wsg` compatibility contract?

Type: research
Status: ready-for-agent
Blocked by: none

## Question

ADR 0009 says the shared Rust library owns "compatibility-sensitive persistence, locking, process reconciliation, and runtime coordination", and that "existing Go wsg state and processes remain compatibility peers until conformance is proven". The same ADR says adopting the domain "does not authorize destructive state conversion".

Nothing there says *which* persisted surfaces are covered. This effort wants to reshape the Pool state — from one pool per Repository to several — and this answer decides whether that is ours to do or a contract to preserve.

Establish, from the repository rather than from memory:

- What the Go `wsg` binary wrote, and what it still reads. `.scratch/wsg-migration/PRD.md` and the compatibility work are the sources of truth; so is `AGENTS.md`'s note that `crates/wsg` is "frozen at its current version while the migration-era binary is retired".
- Which of `.jj/pool/pool.json`, `.jj/pool/worker-*.json`, `waiting.json`, `runs.jsonl`, `undo.json` are read by `wsg` today, and which are jjfx-only.
- Whether a layout change (for example `.jj/pool/<name>/`) would break a compatibility peer, and whether that needs a migration, a dual-read, or nothing.
- Whether `wsg` is still live in practice at all. "Frozen while the migration-era binary is retired" may mean the contract is already historical.

Deliver a markdown summary as a separate linked asset (not pasted here), and a clear verdict: **contract**, **ours**, or **contract with a named migration path**.

## Answer

<!-- recorded on resolution -->
