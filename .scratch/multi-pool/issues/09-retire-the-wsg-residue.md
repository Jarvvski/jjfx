# Retire the dead `wsg` compatibility residue

Type: task
Status: ready-for-agent
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

<!-- recorded on resolution -->
