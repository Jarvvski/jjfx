# Research: is the persisted Pool state a `wsg` compatibility contract?

Asset for *Is the persisted Pool state a `wsg` compatibility contract?* (`.scratch/multi-pool/issues/01-is-pool-state-a-wsg-contract.md`).

**Verdict: ours.** The persisted state is jjfx's to reshape. The contract is dead in policy; its residue is still live in the code, and retiring that is separate, small work.

## The promise, as written

`.scratch/wsg-migration/PRD.md` § Compatibility promise required jjfx to preserve, while Go and Rust `wsg` operated against the same Repository:

- `.jj/ws-cache` byte format
- `.jj/pool.json` shape and optional fields
- Worker state JSON shape, including explicit `null` fields
- Dispatch Group JSON shape
- lock file names and lock scope
- atomic replacement behavior
- Worker Status and Sub-issue Status meanings
- PID and process-group reconciliation behavior
- stdout/stderr discipline, command names, aliases, options, exit behavior

Issue `03-lock-down-compatibility-contracts` refined it: "Existing wire formats are frozen for the migration. **Additive fields are allowed only when old consumers ignore them safely.**"

That clause matters: even under the promise, adding an optional field to the Pool or Worker documents was permitted. What was forbidden was reshaping — moving a file, renaming a lock, changing an existing field's meaning.

## Why it is dead

1. **README § Status is the current authority**: "the Go `wsg` project is **deprecated** — it was archived on 2026-08-19 — and the `wsg` compatibility binary that briefly replaced it has been removed. `jjfx` is the only binary built and released from this repository. Existing Go `wsg` releases stay available, but receive no new development."
2. **`crates/` is empty.** The Rust compatibility `wsg` binary is gone. `Cargo.toml` still refers to "the compatibility binary (which has its own workspace member)" — stale.
3. **The cutover never completed, and no longer needs to.** Every `.scratch/wsg-migration/issues/*` ticket is `resolved` except `25-run-manual-acceptance-and-record-cutover-evidence`, which is `wontfix`.
4. **The owner's ruling, 2026-10-16**: "WSG should be gone/dead."

A peer that is archived, whose replacement binary has been removed, and whose conformance evidence was waived cannot be a peer jjfx must stay compatible with.

## The residue — still live, still constraining

The promise is dead in policy but not yet in the code:

- **`tests/fixtures/compatibility/`** — 15 golden fixtures from Go-created state (`pool-empty.json`, `pool-workers.json`, `worker-idle-claude.json`, `worker-busy-claude.json`, `worker-done-codex.json`, `worker-failed-codex.json`, `worker-legacy-omits-runtime.json`, six dispatch-group states, three ws-cache files). Asserted by `tests/worker_pool.rs`, `tests/state_repositories.rs` and `tests/dispatch_group.rs` through a shared `FIXTURES` constant.
- **`docs/adr/0011-exclude-occupied-workspaces-from-dispatch.md`** cites the promise as a live constraint: "The compatibility promise (`.scratch/wsg-migration/PRD.md`) forbids changing the Go-compatible Pool and Worker documents, so occupancy cannot be a new field on `WorkerState`."
- **`src/engine/state.rs`** documents three jjfx-owned sidecar documents as existing *because* of the promise: `InUseState` (occupied Workspaces), `SessionState` (runtime session per Worker) and the waiting set — each "kept out of the Go-compatible Pool and Worker documents so their shape is preserved. A compatibility peer ignores it."
- **`dist/`** still carries `jjfx-0.50.1-wsg-0.18.0-aarch64-apple-darwin.tar.gz` and its `.sha256`.

## What this means for the design

1. **The layout fork is genuinely open.** A directory per pool (`.jj/pool/<name>/`) is permitted; nothing excludes it now. Choose on merit — locking, migration cost, clarity — not on compatibility.
2. **The jjfx-owned sidecars become foldable.** `InUseState`, `SessionState` and the waiting set exist as separate files *only* to avoid touching a Go-shaped document. With the promise retired they can move into the Worker or Pool document, removing a class of "two files must agree" problems. That is directly relevant: a pool-aware layout has to decide where per-pool state lives anyway.
3. **Nothing that was safe is now unsafe.** Additive fields were already allowed, so the verdict widens the option space rather than invalidating an existing option.
4. **Retiring the residue is separate work.** Filed as *Retire the dead `wsg` compatibility residue* — a small AFK task. It belongs on this map because it is what makes (1) and (2) true in the code rather than only on paper.
