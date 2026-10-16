# Retire the dead `wsg` compatibility residue

Type: task
Status: resolved
Blocked by: none

## Question

Not a decision to make — work that makes a decided thing true. *Is the persisted Pool state a `wsg` compatibility contract?* answered **ours**, but the dead contract's residue is still asserted and still cited as a live constraint:

- `tests/fixtures/compatibility/` — 15 golden fixtures from Go-created state, read by `tests/worker_pool.rs`, `tests/state_repositories.rs` and `tests/dispatch_group.rs` through a shared `FIXTURES` constant.
- `docs/adr/0011-exclude-occupied-workspaces-from-dispatch.md` — cites the promise as the reason occupancy cannot be a `WorkerState` field.
- `src/engine/state.rs` — `InUseState`, `SessionState` and the waiting set are documented as "kept out of the Go-compatible Pool and Worker documents so their shape is preserved. A compatibility peer ignores it."
- `Cargo.toml` — a stale comment about "the compatibility binary (which has its own workspace member)".
- `dist/` — `jjfx-0.50.1-wsg-0.18.0-aarch64-apple-darwin.tar.gz` and its `.sha256`.

Retire it. For each item decide **delete**, **repurpose**, or **reword**, and record which in the answer:

- **The fixtures.** They remain good round-trip fixtures for the *shapes* even though the peer is gone. Repurposing them — dropping the "compatibility" framing while keeping the assertions — may be better than deleting them. Decide; do not assume.
- **ADR 0011.** Its recorded constraint has lapsed, but an ADR is a historical record: prefer superseding it or annotating it over rewriting its Context.
- **The sidecar documents.** Do **not** fold them into the Pool or Worker documents here — that is a layout decision owned by *Where do pools live on disk, and how does today's pool migrate?*. Only fix the doc comments that assert a live compatibility peer.
- **The `dist/` archives.** Check whether anything links to them before removing.

## Answer

**Resolved.** Every item is deleted, repurposed, or reworded — nothing was left asserting a live peer. Recorded per item below.

- **`tests/fixtures/compatibility/` — repurposed.** Kept, not deleted, and renamed to `tests/fixtures/state/`. The framing was dropped (the shared `FIXTURES` constant and the test doc comments no longer call them "compatibility" fixtures), but the assertions stand: the fixtures pin the persisted Pool, Worker, Dispatch Group and sidecar **shapes**, and those are exactly the shapes the multi-pool migration has to read and convert. Deleting them would throw away the only round-trip coverage of the wire documents. Landed in the first commit of this change.
- **`docs/adr/0011-...md` — annotated.** The Context's second constraint ("the compatibility promise … forbids changing the Go-compatible Pool and Worker documents") is left as written; a short paragraph was added after it noting the promise has lapsed (Go archived, the multi-pool effort ruled the state is jjfx's to reshape) so occupancy is no longer barred from `WorkerState`. This follows the inline-annotation style ADR 0009 already uses for ADR 0012.
- **`src/engine/state.rs` — reworded.** The three "exact Go-compatible {Pool configuration,Worker state,Dispatch Group state}" doc comments became "exact persisted …". The five sidecar sites (`InUseState`, `SessionState`, the waiting set, the Run Record, `UndoState`) keep the "deliberately kept out of the Pool and Worker documents so their shape is preserved" clause and drop the "A compatibility peer ignores it" sentence. Genuinely generic uses ("forward-compatible persisted status value", "the compatible set of mutations", "under the compatible Pool lock") were left alone — they name jjfx's own forward-compatibility, not a peer.
- **`Cargo.toml` — reworded.** The `exclude` comment lost the "compatibility binary (which has its own workspace member)" clause; the `rustix`/`tempfile` comment lost "Absorbed with wsg-core (the crate is gone; its engine lives in src/engine)"; the `serde_json` comment's "Go-compatible numeric fidelity" became "exact numeric fidelity".
- **`dist/` archives — nothing to do.** `dist/` does not exist in the tree (never tracked), and nothing references the old tarball names. Verified by search; no removal needed.
- **`src/engine/mod.rs` — reworded.** The module header's "Absorbed from the former `wsg-core` crate so `jjfx` ships as a single crate" became a plain statement that the engine lives in this crate.
- **`CONTRIBUTING.md` — reworded.** The "Where things live" row lost "the absorbed former `wsg-core`".
- **`AGENTS.md` — deleted.** The sentence about `crates/wsg` being frozen was removed; `crates/` is empty and untracked.
- **`README.md` — deleted.** The whole `## Status` section (whose only content was the `wsg` deprecation notice) was removed; there is no status to state now.
- **`docs/adr/0009-...md` — annotated.** A short paragraph after the Context records that the migration is retired and `.scratch/wsg-migration/PRD.md` no longer exists, so the pointer is dead. The Context is kept as history.
- **`.scratch/wsg-migration/` — deleted.** All 30 files (PRD, 4 epics, 25 issues) removed: a plan for a migration that no longer exists.

Beyond the enumerated residue, the same dead-format claim turned up in the engine's remaining doc comments and in three reference docs, and each was reworded because leaving it would keep asserting a live peer. In `src/engine/`: `mod.rs` ("Go-compatible Workspace path"), `watchdog.rs` ("Go-compatible Pool and Worker documents"), `workspace.rs` ("Go-compatible idempotent behavior", "Go-compatible Worker Workspace"), and `pool.rs` ("Go-compatible number of reusable Worker slots", "Go-compatible Worker Pool"). In the docs: `docs/undo.md` and `docs/run-records.md` (dropped the "A compatibility peer ignores it" sentence and the "Go-compatible"/"compatible" qualifiers), `docs/configuration.md` (dropped two "Go-compatible Pool and Worker documents" qualifiers), and `skills/jjfx-dispatch/SKILL.md` (dropped the parenthetical claiming "the `wsg` binary exposes the same commands"). Nothing else in those files changed.

Left deliberately alone: `CHANGELOG.md` (release history — expunging it would falsify what shipped); other efforts' `.scratch/*/` tickets that mention `wsg` only in passing; and `docs/adr/0010-colocate-workspaces-with-git.md`, which names the `wsg`/`jjfx` CLI in a Decision bullet — a passing binary-name mention in a historical ADR with no live constraint to annotate. ADR 0009's and ADR 0011's own Context text keeps its `wsg` references on purpose: the annotations above are the mechanism, and the history stays readable.
