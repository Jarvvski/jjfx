# Bug hunt: correctness and resilience sweep

Status: reopened (2026-10-16) for ticket 42 - 40 resolved, 1 `wontfix` (06).

## Notes

Scope: jjfx at 0.52.1 plus the occupancy change (`main` = `86227600f0ea`).

Four sweeps were run over the tree - the engine (concurrency / persistence /
processes), the TUI and event plumbing, the forge / `jj` / `gh` seams, and the
Worker Dispatch domain - plus one external report
(`@/tmp/jjfx-worker-cwd-bug.md`).

Method: **verification-first.** Every candidate becomes a failing test that
encodes the *domain* expectation before any fix. Fix TDD, one focused commit,
`mise run check` green, a dated `CHANGELOG.md` entry with a semver marker, and
land **locally only** (never push - `CLAUDE.md`).

Confidence key:

- **verified** - read in source by the lead.
- **sweep** - from a subsystem sweep; needs a failing test to confirm.
- **refuted** - a sweep finding checked and disproved.
- **suspicion** - a design/intent question, not yet a defect.

Guardrails to add while hunting: enable the clippy `unwrap_used` /
`indexing_slicing` lints (the "no unwrap outside tests" rule is convention-only
today), and add a **colocated-repo** fixture and a **launch-from-secondary-
workspace** fixture, since CI is `ubuntu-latest` only and both are blind spots.

## Findings ledger

Severity: H high, M medium, L low. Conf: V verified, S sweep, ? suspicion.

| ID | Area | Sev | Conf | Finding | Ticket |
| --- | --- | --- | --- | --- | --- |
| W1 | dispatch | H | report | Worker Run inherits the caller's cwd, not its Workspace | 01, 05 |
| F2 | process | H | report | an opencode Run's recorded pid is a daemon client: stopping it does not stop the work, and it outlives the Run | 37 (fixed) |
| D1 | dispatch | H | V | A failed Sub-issue never unblocks dependents; the group never terminates and `run` polls forever | 04 (fixed) |
| D3 | work | - | refuted | git-exported bookmarks count as "pushed" - `remote_bookmarks()` already excludes `@git` | 06 |
| C1 | concurrency | H | S | `send`/`review`/`rebase` skip the workspace-op lock; Reset's async restore races a new Run | 08 (fixed) |
| C2 | process | H | S | PID reuse can TERM/KILL an unrelated process group on Reset/destroy | 07 (fixed) |
| T1 | TUI | H | S | blocking kitty subprocess + ~5s sleep on the UI thread | 17 (fixed) |
| T2 | TUI | H | S | help over Detail/Graph renders nothing yet swallows every key | 18 (fixed) |
| I1 | forge | H | S | every `gh` failure is reinterpreted as "no origin remote" | 10 (fixed) |
| C3 | state | M-H | S | `.cleanup` marker written before the pool manifest permanently blocks a Worker | 09 (fixed) |
| C4 | process | M | S | foreground launch failure kills only the direct child; foreground Runs aren't their own group | 20 (fixed) |
| C5 | state | M | S | `detached_cleanup_markers` aborts on any malformed `.cleanup` filename | 20 (fixed) |
| C6 | process | M | S | `cleanup_untracked_run` can release a reservation while the group is alive | 20 (fixed) |
| D2 | dispatch | M | V | branch repair hardcodes `adam/` while dispatch derives the prefix from `user.name` | 14 (fixed) |
| D4 | dispatch | M | S | orchestration freezes occupancy exclusions for the whole run (violates ADR-0011) | 15 (fixed) |
| D5 | dispatch | M | S | retry path resets the Worker before persisting the group; a save conflict strands it | 16 (fixed) |
| I2 | forge | M | S | PR read failures degrade to "no PRs"; `--limit 100` drops older PRs | 29 (fixed) |
| I3 | forge | M | S | conflict detection only inspects `@`; a lower-commit conflict welds and pushes | 10 (fixed) |
| I4 | forge | M | S | `pushed_something` sniffs the English string `"Nothing changed."`; a failed push reads as no-op | 10 (fixed) |
| I5 | store | M | S | `default` workspace pinned to the current workspace when launched from a secondary workspace | 11 (fixed) |
| I8 | dispatch | M | S | untrusted Linear text injected verbatim into prompts | 12 (fixed) |
| T3 | events | M | S | one invalid UTF-8 byte wipes all startup agent state | 19 (fixed) |
| T4 | events | M | S | replay/tailer offset race drops events | 19 (fixed) |
| T5 | events | M | S | rotation only at startup; unbounded `events.jsonl` growth + rotation race | 19 (fixed, residual race documented) |
| T6 | TUI | M | S | blocking `jj tidy`/`tidyws` on the UI thread | 17 (fixed) |
| T7 | TUI | M | S | per-tick `canonicalize` and duplicate graph/row projection per frame | 26 |
| I6 | work | M-L | S | workspace names interpolated unquoted into revsets | 22 |
| I7 | workspace | M-L | S | two divergent ad-hoc workspace path rules (CLI vs TUI) | 23 |
| I9 | dispatch | M-L | S | duplicate Ticket ids are never deduped -> double dispatch | 13 (fixed) |
| I10 | run log | L-M | S | any invalid cost line turns a successful Run into a failure | 28 |
| C7 | process | L-M | S | a recycled PID keeps a Worker `busy` forever | 07 (fixed) |
| D6 | attention | L | ? | a newer local/pushed change masks a changes-requested PR (stacked work) | 28 |
| D7 | dispatch | L | ? | mixed merged+unmerged blockers can set the PR base to `main` | 28 |
| D8 | dispatch | L | ? | non-todo dependency statuses are skipped but unblock | 28 |
| D9 | run log | L | S | Claude Run results never carry token usage | 28 |
| C8 | state | L | S | `DispatchGroupStateRepository::path` lowercases the parent id -> case collision | 28 |
| T8 | TUI | L-M | S | no SIGINT/SIGTERM handler -> terminal left in raw/alt-screen | 27 |
| T9 | TUI | L-M | S | input reader thread silently dies on the first read error | 27 |
| I11 | mixed | L | S | wrong CLI capacity message; poller ignores the Store projection; `reload` resets `colocate`; `config_path` relative fallback | 28 |
| T10 | TUI | L | S | `?` steals from text buffers; help-over-editor hides the editor; `viewport` `u16` truncation; `line_down` overflow | 28 |
| T11 | TUI | H | report | `jjfx tui` floods the terminal with unbounded, un-deduplicated `kitten @ ls` subprocesses until kitty pegs a core | 40 (fixed) |
| T12 | TUI | H | report | the recursive `.jj` watcher and a snapshotting Workspace projection form a self-sustaining reload loop that storms `jj` and everything downstream of a reload | 41 (fixed) |
| T13 | process | L | verified | the engine's two `jj git remote list` identity reads also snapshot the working copy, committing it as a side effect of dispatching | 41 (fixed) |
| F4 | tests | M | verified | a real `jj workspace add` in the cache-provisioning test blocks past the shared 120s budget under heavy load (2 sightings) | 43 |
| F3 | tests | M | verified | the process- and exec-timing integration tests flake under CI and load (an empty PID-file read; an ETXTBSY holder race) | 42 |

## Tickets

### Phase 0 - repro harness (build the red tests first)

- [01](issues/01-reproduce-worker-run-cwd.md) - research: reproduce the Worker
  cwd report and pin the root cause. **resolved** - not reproducible; guard
  test landed.
- [02](issues/02-regression-harness-colocated-and-secondary.md) - task:
  regression tests for colocated "pushed" (D3) and the secondary-workspace
  default root (I5). **resolved** - D3 refuted, I5 tested with its fix.
- [03](issues/03-regression-harness-liveness-and-op-race.md) - task:
  regression tests for the failed-dependency loop (D1) and the Reset/`send` race
  (C1).
  **resolved** - tests-only; both guards proved red by reverting the fix they
  pin (04's quiescence predicate, 08's operation lock on the `rebase` half).

### Phase 1 - high severity

- [04](issues/04-terminate-group-on-failed-dependency.md) - D1.
  **resolved** - quiescence + `blocked` count.
- [05](issues/05-fix-worker-run-working-directory.md) - W1 (blocked by 01).
  **resolved** - closed as an upstream opencode session-directory defect, not a
  jjfx one: not reproducible on opencode 2.0.27, and jjfx's cwd is correct in
  v0.52.1's own tree. Finding 2 (an opencode Run is a daemon client) is carried
  into 07.
- [06](issues/06-exclude-git-exported-bookmarks.md) - D3 (blocked by 02).
  **wontfix** - refuted.
- [07](issues/07-durable-run-identity.md) - C2, C7.
  **resolved** - option 3: group-based liveness (a recorded pid counts as live only
  while it leads a process group), landed with 20/C4.
- [08](issues/08-serialize-workspace-operations.md) - C1 (blocked by 03).
  **resolved** - Follow-ups and rebases take the lock.
- [09](issues/09-order-cleanup-marker-after-manifest.md) - C3.
  **resolved** - manifest-first ordering.

### Phase 2 - medium

- [10](issues/10-forge-failure-handling.md) - I1, I3, I4.
  **resolved** - typed push outcome, chain-wide conflict check, surfaced PR context.
- [11](issues/11-normalize-repository-root.md) - I5 (blocked by 02).
  **resolved** - root normalised at discovery.
- [12](issues/12-fence-untrusted-ticket-content.md) - I8.
  **resolved** - fenced + slug validation.
- [13](issues/13-deduplicate-dispatch-tickets.md) - I9.
  **resolved** - deduped at discovery and at batch reservation.
- [14](issues/14-single-source-branch-prefix.md) - D2.
  **resolved** - resolution matches on the Ticket id.
- [15](issues/15-live-occupancy-exclusions.md) - D4.
  **resolved** - occupancy re-queried per claim via an `ExclusionsProvider`.
- [16](issues/16-persist-group-before-reset.md) - D5.
  **resolved** - persist-before-reset in the retry path.
- [17](issues/17-move-blocking-work-off-ui-thread.md) - T1, T6.
  **resolved** - tab opens, tidy sweeps, and `Store::reload` (ticket 30) all run
  as effects; `Jj` moved from the App to the runtime.
- [18](issues/18-help-overlay-in-detail-and-graph.md) - T2.
  **resolved** - overlay drawn over Detail/Graph.
- [19](issues/19-event-log-tolerance-and-tailing.md) - T3, T4, T5.
  **resolved** - lossy line reader, replay offset, in-tailer rotation.
- [20](issues/20-workspace-cleanup-robustness.md) - C4, C5, C6.
  **resolved** - C5 fixed; C4 landed as own process group plus a child-side terminal
  handback (no `unsafe`, no new crate), and C6 by injecting the cleanup outcome.

### Phase 3 - improvements

- [21](issues/21-typed-jj-access-layer.md) - one jj seam with typed outcomes.
  **resolved** - jj semantics are classified from machine-readable state (jj
  templates, `jj log --count`, the Git-format diff, remote-bookmark snapshots)
  and a jj failure is a typed error, not a silent zero/empty; `parse_diff_stat`
  and the `"Nothing changed."` sniff are gone. One documented exception:
  `forge::config_value` still folds a jj error into "unset", because
  `jj config get` exits 1 for both cases (verified directly).
- [22](issues/22-quote-refs-and-validate-workspace-names.md) - I6.
  **resolved** - `jj::revset_symbol` quotes every revset/diff builder, and the
  workspace list is read via `stringify(name)` so a quoted name still matches
  the ws-cache. (A second root cause: the `name` symbol template.)
- [23](issues/23-unify-ad-hoc-workspace-paths.md) - I7.
  **resolved** - `ad_hoc_workspace_path` delegates to the shared `workspace_base`,
  so TUI create, CLI add, and Worker provisioning agree; existing workspaces are
  cache-preserved and keep their real paths.
- [24](issues/24-jj-lib-jj-version-compat-check.md) - version coupling.
  **resolved** - a `jj` on `PATH` that differs from the `jj-lib` target is surfaced
  (world graph, TUI footer, CLI stderr) instead of silently degrading; the target
  is a source constant asserted against `Cargo.toml` by a test.
- [25](issues/25-consolidate-state-write-and-lock-helpers.md) - one atomic
  write/lock helper.
  **resolved** - `engine/atomic.rs` holds `write_atomic_bytes` +
  `lock_exclusive`/`try_lock_exclusive`; the ws-cache write now conforms to the
  documented contract (unique temp name, mode 0644, cleaned up on failure).
- [26](issues/26-tui-per-frame-work-and-agent-state-cache.md) - T7.
  **resolved** - idle ticker gating, one graph layout per frame, and a
  canonicalised agent-path memo.
- [27](issues/27-terminal-restore-on-signal-and-input-resilience.md) - T8, T9.
  **resolved** - SIGINT/SIGTERM/SIGHUP restore the terminal via a `signal-hook`
  thread; the panic hook is installed before the terminal is touched; a failed
  input read is retried when interrupted and surfaced otherwise.
- [28](issues/28-misc-low-severity-fixes.md) - I10, C8, D6, D7, D8, D9, I11, T10.
  **resolved** - T10c fixed at triage; I10 (with the authorised API break), I11a,
  I11b, I11c, and T10a landed as five focused commits; C8 and T10b refuted;
  D6-D9 escalated to 31-34.
- [29](issues/29-distinguish-pr-read-failure.md) - I2 (split out of 10).
  **resolved** - a failed PR read yields `Unknown`, not `Pushed`.
- [30](issues/30-background-store-reload.md) - the `Store::reload` half of 17.
  **resolved** - reloaded via `Effect::ReloadStore`.
- [31](issues/31-attention-precedence-on-stacked-work.md) - D6, split out of 28.
  **resolved** - option 1: a changes-requested PR outranks work stacked above it;
  `overlay` returns early only for a change request, and CONTEXT is amended.
- [32](issues/32-mixed-merged-and-unmerged-blockers.md) - D7, split out of 28.
  **resolved** - option 1: the first non-`main` branch, so a merged blocker
  cannot silently rebase a dependent onto trunk.
- [33](issues/33-non-todo-dependencies-unblock.md) - D8, split out of 28.
  **resolved** - option 1: block until Done/Merged; in-flight and terminal
  undelivered statuses no longer satisfy dependents, and `unblocks()` is gone.
- [34](issues/34-claude-result-usage.md) - D9, split out of 28.
  **resolved** - option 1: populate the Run result's usage, matching Pi and Codex.
- [35](issues/35-load-sensitive-helper-deadlines.md) - the test suite's
  load-sensitive waits, found while landing 03/23/28 and re-opened when the
  class recurred in a new `engine::locks` test.
  **resolved** - one shared `test_support::LIVENESS_TIMEOUT` (120s), defined in
  the crate so unit and integration tests share it, replaces every private
  liveness budget (`locks.rs`/`app.rs`/`wait.rs`/`logs.rs` 30s,
  `agent_runtime.rs` 10s, `repository.rs` inline 120s), and the locks ordering
  test now waits out a one-second observation window and requires the pool lock
  to *clear* rather than never reading `Busy` - the recurrence was that
  momentary-`Busy` assertion, not the 30s budget. 90 consecutive full-suite runs
  under load failed 0 (pre-fix roughly 1 in 8). No changelog entry: nothing
  user-visible.
- [36](issues/36-fixed-sibling-paths-collide.md) - fixed sibling paths in
  `store.rs` tests, found while landing 28.
  **resolved** - siblings derive from the unique repo directory via a `sibling`
  helper; 39/48 invocations failed pre-fix under concurrency, 132/132 passed
  after. Same class as 35 but a different mechanism: isolation, not deadlines.
  No changelog entry: nothing user-visible.
- [37](issues/37-stopping-a-worker-does-not-stop-its-work.md) - F2, split out of
  05. **resolved** - stop through `session.interrupt` and reconcile on the session
  `outcome`; identified by the id 38 records in the sidecar, with title matching
  only as a guarded fallback. Verified live against a real daemon-hosted Run.
- [38](issues/38-set-run-session-directory.md) - guard for the ticket 05 class.
  **resolved** - `session.create` with `location.directory` = the Worker Workspace,
  read back and compared before the Run starts, then driven with `--session <id>`;
  the id is recorded in a jjfx-owned sidecar (`.jj/pool/sessions.json`) for 37.
  Unavailable API falls back to the process cwd and never fails a Run.
- [39](issues/39-opencode-api-client.md) - shared prerequisite for 37 and 38.
  **resolved** - `src/engine/opencode_api.rs`: a typed, best-effort client over
  `opencode api` with an explicit unavailable-versus-rejected contract and an
  injectable runner seam. 12 tests, no live server; no dependency, no `unsafe`.
- [40](issues/40-kitty-ls-storm.md) - T11, from an external report (2026-10-14).
  **resolved** - the terminal query is bounded (3s) and single-flighted with a 2s
  TTL, so a slow kitty can neither block unboundedly nor stack subprocesses. The
  stub-`kitten` regression test proves the pre-fix storm red (peak 16 → 1).

- [41](issues/41-jj-reload-loop.md) - T12, the *primary* finding of the same
  external report 40 was filed from; 40 fixed the symptom, this is the cause.
  **resolved** - the Workspace projection passes `--ignore-working-copy`, so it
  can no longer snapshot the working copy and ping its own watcher. The
  regression test proves the snapshot red first. The report's secondary
  recommendations (the bounded, cached `ls`) had already landed with 40.

- The sweep finding D3 (git-exported bookmarks counted as "pushed") is
  **refuted**: `remote_bookmarks()` already excludes the colocated `git` remote.
  The sweep misread the `remote_bookmarks` template keyword as the revset
  result. Verified with `jj 0.46.0` on a colocated repo with an unpushed
  bookmark. This is why every sweep finding is tested before it is fixed.

- The sweep finding C8 (case-collision on Dispatch Group files) is **refuted**:
  tickets come from Linear (canonical uppercase `TEAM-NUMBER`) with no
  file-based discovery, the lowercasing is *mandated* by the wsg compatibility
  contract (`docs/wsg-compatibility-contract.md:58,111`) and pinned by
  `tests/state_repositories.rs:93-113`, and APFS is case-insensitive so the
  filenames collide with or without the lowercase. Only a confusing fail-closed
  error remains when the *same* ticket is entered with inconsistent casing.

- The sweep finding T10b (help over the task editor hides it and drops paste) is
  **refuted** as a user-facing bug: `Help(Box::new(Pool(SendInput)))` is
  unreachable, because `on_key` returns into the editor before the `?` handler
  that is the only constructor of `Mode::Help`, and
  `pool_send_editor_submits_a_prompt_without_blocking_input` pins that. The
  three direct `SendInput` comparisons are a latent inconsistency with
  `pool_mode()`; recorded as optional hardening in ticket 28.

### Reopened - 2026-10-16

- [42](issues/42-flaky-process-and-exec-tests.md) - F3: the process- and
  exec-timing integration tests flake under CI and load. `ready-for-agent`.

## Decisions so far

- The `wsg` compatibility contract **explicitly excludes** parent-directory
  fsync (`docs/wsg-compatibility-contract.md:137`), so `write_atomic`'s missing
  directory fsync is not a defect to file. Ticket 25 may still add it as a
  documented improvement.
- PID-as-identity is acknowledged in the contract ("liveness hints, not Run
  identity", `:147`). The fix (ticket 07) is a durable Run token, **not** a
  change to the frozen Worker document. **Decided 2026-10-14 (option 3):** the
  current kill paths already only signal process *groups*, so the residual risk is
  narrow - a recycled pid that does not lead a group yields `ESRCH` and kills
  nothing. Group-based liveness closes the common case (a recycled pid that does
  *not* lead a group) with no new dependency and no `unsafe`, and is sound once
  20/C4 gives every Run its own process group. The residual - a recycled pid that
  *does* lead a group - is narrowed, not closed. The Pool view's display probe was
  aligned with it too, so the two agree for the runtimes whose PID owns the work;
  an opencode Run is judged from its session instead (ticket 37), because its PID
  is only a daemon client.
- Two findings were test-encoded current behaviour - non-todo dependency statuses
  unblocking (D8), and Claude usage omission (D9). **Both decided 2026-10-14**
  (tickets 33 and 34, option 1 each): in-flight statuses now block until
  Done/Merged, and Claude's result usage is populated to match Pi and Codex.
- The Worker cwd report (W1) is **not reproducible** against `main` or
  `v0.52.1`: jjfx sets the Run cwd correctly (guard test added) and opencode
  honours it, including in a colocated worktree. Ticket 05 is `needs-info` with
  a recurrence diagnostic; the leading untested variable is the reporter's
  `PATH` (`opencode` may be a wrapper).
- The second local review pass (`6c62a5..main`: tickets 37, 38, 40) is
  **applied**. Fixed: the duplicated session identification is now one
  `identify_run_session` helper shared by the stop path and reconciliation; the
  Run log is read once and its provider result finishes a Run even while the
  session is still open - the decision's `outcome` *or* log-result, which an
  open session was silently short-circuiting; the Pool view no longer labels an
  opencode Worker "stale" on its dead client pid; the lock protocol lists the
  sessions sidecar. Declined, with reasons: a `SessionId` newtype (the id is
  opaque and only passed through), bundling `(worker, ticket)` (already carried
  by `RunTarget` where it matters), and narrowing `create_run_session`'s error
  to `OpenCodeError` (every caller only renders it). The stale-id residual
  reported with ticket 37 is now **closed**: a Run that drives no session
  forgets the previous one, so the sidecar cannot finish the wrong Run.

## Fog

- How much of the jj read path can move to `jj-lib` without breaking the version
  coupling - still live, parked until the churn gate clears; tracked in
  [`01-migrate-reads-to-jj-lib`](../jj-lib-reads/issues/01-migrate-reads-to-jj-lib.md).

### Filed - 2026-10-19

- [43](issues/43-concurrent-provisioning-blocks-under-load.md) - F4, filed from two
  independent sightings while fixing `35` and `42`; the third load-sensitive
  failure, and the first where the work itself did not complete.
  **resolved** - not a shared cache: the hypothesis is ruled out (per-copy
  `TMPDIR` isolation does not change the scaling, and neither does bypassing the
  `mise` shim), there is no deadlock or lock-ordering bug, and the blocking is
  not reproducible (~350 runs under load, including the 42 harness, 0
  failures). The one genuinely shared, serialised resource in the path is the
  developer's GPG smartcard, which `jj workspace add` uses to sign the
  workspace's working-copy commit whenever `JJFX_TEST_SIGNING_OFF` is unset;
  `mise run test` sets it. Harness reconstructed at
  `.scratch/bug-hunt/load-harness.sh`.

### Filed - 2026-10-20

- [44](issues/44-concurrent-operations-diverge-the-op-log.md) - F5, filed from a
  sighting while implementing `dogfood/13`: concurrent `jj` operations across
  Workspaces made jj report `reconcile divergent operations` and swallowed a
  worker's commit into a divergent head. The worker noticed and re-applied the
  edit, so nothing was lost - but only because it read its own history. jjfx is
  built to run concurrent jj operations against one shared operation log, and jj
  attributes an operation to no tool or process, so a lost commit has no
  attribution. Reproduce or rule out; if real, either serialise or report it.
  **resolved** - reproduced, and jjfx caused it: the rollback `jjfx` ran after a
  failed push was `jj undo --allow-cross-workspace`, and a repository-wide undo
  restores every Workspace's working-copy commit, so racing a sibling's own
  commit left that sibling's change divergent (two commits, one change id) with
  nothing reported. The rollback is now a plain `jj undo`, which jj refuses to
  cross Workspaces - so it can only undo this Workspace's operation and reports
  the refusal otherwise. Harness at
  [`.scratch/bug-hunt/op-log-harness.sh`](op-log-harness.sh). Ruled out (no
  divergence under load): a single flagged undo, `lift_all`, `tidy`,
  `jj op restore`, and `tidy`+`lift_all` together. jjfx still cannot *tell* a
  Worker its commit diverged or went hidden - filed as 46.

### Filed - 2026-10-21

- [46](issues/46-a-worker-is-never-told-its-commit-diverged.md) - from 44's last
  acceptance box: jjfx reads the operation log but never reads jj's `divergent()`
  revset, so a Worker whose commit has diverged or gone hidden is told nothing.
  44 removed jjfx's own path to causing it; this is the capability to report it.
  **resolved** - `jjfx status` now reads divergence from jj's `divergent()` revset
  over the Workspace's own chain and reports it: a `diverged:` line on the
  Worker's row and a `diverged` array in `status --json` naming the Worker, its
  pool, the change id and how many visible commits share it. A commit rewritten
  out of the visible set reads as intact and jjfx says so - it can tell a commit
  diverged, not that it is gone. The test drives a real divergence
  deterministically (two operations from one starting operation, both rewriting
  the working-copy commit, then integrated) rather than racing.

### Filed - 2026-10-22

- [47](issues/47-a-cap-fixture-rots-outside-the-rolling-week.md) - found while
  landing `44` (#188): the spend fixtures (`pool_routing`'s capped-lane case and
  all eight of `pool_spend`) seed a Run with a hard-coded `completed_at` and rely
  on a cap refusing, but the default spend window is a rolling seven days - so
  once the date moved on the Run aged out, the cap stopped refusing, and the
  fixtures silently stopped describing what they assert. Deterministic, not
  flaky: 30/30 locally, and CI failed on 2026-10-22 having passed the same commit
  on 2026-10-21. `main` is red from 2026-10-22 onward. The two surfaces look like
  different bugs - `pool_routing` fails with `state is missing` (see 48), while
  `pool_spend` fails with the terminal refusal because its *deliberately failing*
  `kitten` stub is only meant to be exercised by a held batch. **resolved** - the
  fixtures now stamp the seeded Run with `support::recent_timestamp()`, which
  keeps the default rolling week in play; falsified both ways (original fails,
  original with a fresh timestamp passes, the fix passes: `pool_routing` 11/11,
  `pool_spend` 8/8 in 2.3s, down from 120s).

- [48](issues/48-a-pool-directory-without-a-document-is-not-materialised.md) -
  the latent defect 47 uncovered. `materialise` treats "the state *directory*
  exists" as "the pool is materialised", and that directory also holds Run
  records and locks - `atomic::open_lock` creates its parents - so a directory
  without a document is enough to skip provisioning, after which the first
  reservation fails with `state is missing`, naming neither the pool nor the
  missing document. **resolved** - reproduced on a product path: the TUI's
  in-place destroy (`WorkerPool::destroy`, which deliberately leaves the
  directory) leaves `.jj/pools/<name>/` holding only locks, so the next Dispatch
  skipped materialisation and failed; `materialise` now keys off the pool
  *document* (`StateStore::pool_document_exists`), and the residual reservation
  error names the pool and the missing document. Pool-scoped reads,
  `pool create`, the CLI `pool destroy`, and the legacy conversion were ruled
  out as the cause.
