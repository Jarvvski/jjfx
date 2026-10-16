# Is the persisted Pool state a `wsg` compatibility contract?

Type: research
Status: resolved
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

**Verdict: ours.** The persisted Pool state is jjfx's to reshape.

The promise in `.scratch/wsg-migration/PRD.md` is dead in policy and its constraint has lapsed: the Go `wsg` project was archived 2026-08-19 (README § Status), the Rust compatibility binary has been removed (`crates/` is empty), the migration's cutover-evidence ticket is `wontfix` while every other migration ticket is resolved, and the owner ruled on 2026-10-16 that "WSG should be gone/dead".

Two findings worth carrying forward:

- **Even under the promise, additive fields were allowed.** Issue 03's decision was "existing wire formats are frozen for the migration; additive fields are allowed only when old consumers ignore them safely". Adding a `pool` field was therefore always permitted — what was forbidden was reshaping.
- **The residue is still live.** `tests/fixtures/compatibility/` (15 fixtures, asserted by three test files), ADR 0011 (which cites the promise as a live constraint), and three jjfx-owned sidecar documents in `src/engine/state.rs` — `InUseState`, `SessionState`, the waiting set — that exist *only* to avoid touching the Go-shaped documents. Until those are retired, "ours" is true on paper and not yet in the code.

Full evidence and reasoning: [`assets/01-wsg-compat-contract.md`](../assets/01-wsg-compat-contract.md).

Graduated *Retire the dead `wsg` compatibility residue* (task, AFK). The state-layout ticket now has more room than it assumed, including folding the jjfx-owned sidecars back into the Pool and Worker documents.
