# Where do pools live on disk, and how does today's pool migrate?

Type: grilling
Status: resolved
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

Resolved with the owner, 2026-10-16. Four decisions.

**1. A directory per pool, plus a small index.** `.jj/pools.json` lists the pools that exist; `.jj/pools/<name>/` holds everything pool-scoped — the pool document, its Workers, Run Records, the waiting set, occupancy, sessions and its locks. The split is the principle: **pool-scoped state lives under the pool, repository-scoped state at the root.** So `.jj/ws-cache` and `undo.json` stay at `.jj/` — undo because it is `jj op restore`, refused unless the mutation is still the repository's tip, "because the restore is repository-wide". This also collapses today's awkward `.jj/pool.json` file sitting beside a `.jj/pool/` directory.

**2. The jjfx-owned sidecars stay files.** `sessions.json` per pool (it is per-Worker); `in-use.json` at the repository root (occupancy is one reconcile over all the terminal's tabs, and spans pools). The deciding fact is shape: occupancy is written **wholesale**, so as a file it is one atomic replace and as per-Worker fields it would be N read-modify-writes with no single point of consistency. The glossary's insistence that the Agent and Work lifecycles stay orthogonal points the same way.

**3. Automatic one-time conversion on first access.** Detect the legacy layout, build the new one, verify it, then remove the old — one code path afterwards, and it works out of the box on an existing Repository. Safe because the Go peer is dead (ticket 01's ruling) and because the conversion copies before it removes and refuses if it cannot verify the result. ADR 0009's "does not authorize destructive state conversion" was written to protect that peer; "never destroy a Worker Pool" survives it on its own merits.

**4. Locks are both, repository outermost.** Per-pool locks inside each pool directory for dispatch, workers, runs, waiting and sessions; a repository-wide lock for Workspace creation and removal, `ws-cache`, undo, `clean` and colocation. The ordering rule — repository lock outermost, never pool-then-repository — is the part that must be written down, or two pools provisioning at once deadlock.

`jjfx pool <N>` keeps sizing `default`, per the additive stance.

Landed as **ADR 0013**, which records the scoping principle, the lock ordering and the one-way migration. This ticket graduates *Is the pool index or the pool directories the source of truth?* — a directory and an index that can disagree is a recovery question this ticket did not settle.
