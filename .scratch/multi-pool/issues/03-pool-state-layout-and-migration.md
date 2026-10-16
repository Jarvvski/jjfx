# Where do pools live on disk, and how does today's pool migrate?

Type: grilling
Status: ready-for-human
Blocked by: 01, 02

## Question

Decide the persisted shape. The candidates:

- **A directory per pool** — `.jj/pool/<name>/pool.json`, `worker-*.json`, `waiting.json`, `runs.jsonl` — with an index at `.jj/pool/pools.json`. Cleanest, but reshapes every existing path.
- **A `pool` field on each record** — keep the flat layout, tag each Worker and Run Record with its pool. Least disruptive, but "list the pools" becomes a scan and a pool's own configuration has nowhere obvious to live.
- A hybrid — decide which.

Whichever is chosen, answer:

- **Migration.** Every existing Repository has exactly one unnamed pool. Renaming it `default` is the obvious move — confirm it, and say whether migration is a one-time rewrite, a read-time fallback, or both. Existing repositories include jjfx itself and mono.
- **What `jjfx pool <N>` means afterwards.** It sizes `default`, per the additive stance.
- **Concurrency and locking.** Two pools mean two sets of Workers provisioning Workspaces in one Repository. The state store already holds `.dispatch.lock`, `.in-use.lock` and `.runs.lock` — are these per-pool, per-repository, or both? Getting this wrong is a race between pools, which is worse than a race within one.

Blocked by the `wsg` compatibility research — if the layout is a contract, this is a migration design rather than a free choice — and by the domain model, which decides whether a Worker belongs to a pool or is assigned to one.

**That first blocker has resolved, and it widened this ticket.** The compatibility contract is dead (*Is the persisted Pool state a `wsg` compatibility contract?* → **ours**), so the directory-per-pool option is no longer excluded by anything. More usefully, three jjfx-owned sidecar documents — `InUseState`, `SessionState` and the waiting set in `src/engine/state.rs` — exist *only* to avoid touching the Go-shaped documents, and can now be folded into the Pool or Worker documents. Decide whether they should be, since a pool-aware layout has to place per-pool state somewhere anyway. *Retire the dead `wsg` compatibility residue* clears the fixtures and doc comments that still assert the old shapes; this ticket owns the design decision, not that cleanup.

## Answer

<!-- recorded on resolution -->
