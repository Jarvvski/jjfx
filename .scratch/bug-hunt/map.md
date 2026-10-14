# Bug hunt: correctness and resilience sweep

Status: open

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
- [35](issues/35-load-sensitive-helper-deadlines.md) - the test suite's fixed
  helper-subprocess deadlines, found while landing 03/23/28.
  **resolved** - one shared `HELPER_LIVENESS_TIMEOUT` (120s) in
  `tests/support/mod.rs`, documented as a hang guard; the helper exceeded the old
  10s budget in 10 of 12 measured runs, so the budget was the defect, not the
  code. No changelog entry: nothing user-visible.
- [36](issues/36-fixed-sibling-paths-collide.md) - fixed sibling paths in
  `store.rs` tests, found while landing 28.
  **resolved** - siblings derive from the unique repo directory via a `sibling`
  helper; 39/48 invocations failed pre-fix under concurrency, 132/132 passed
  after. Same class as 35 but a different mechanism: isolation, not deadlines.
  No changelog entry: nothing user-visible.
- [37](issues/37-stopping-a-worker-does-not-stop-its-work.md) - F2, split out of
  05. **ready-for-agent** - decided 2026-10-14: stop a Run with
  `session.interrupt`, and treat an opencode Run as finished on the session
  `outcome` or a run-log result rather than on the client pid. Identify the session
  by the id jjfx creates in 38; title matching is only a fallback. **Sequence: after
  07/20 and after 38** - it changes the same functions as 07/20 and builds on 38.
- [38](issues/38-set-run-session-directory.md) - guard for the ticket 05 class.
  **resolved** - `session.create` with `location.directory` = the Worker Workspace,
  read back and compared before the Run starts, then driven with `--session <id>`;
  the id is recorded in a jjfx-owned sidecar (`.jj/pool/sessions.json`) for 37.
  Unavailable API falls back to the process cwd and never fails a Run.
- [39](issues/39-opencode-api-client.md) - shared prerequisite for 37 and 38.
  **resolved** - `src/engine/opencode_api.rs`: a typed, best-effort client over
  `opencode api` with an explicit unavailable-versus-rejected contract and an
  injectable runner seam. 12 tests, no live server; no dependency, no `unsafe`.

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
  *does* lead a group - is narrowed, not closed.
- Two findings were test-encoded current behaviour - non-todo dependency statuses
  unblocking (D8), and Claude usage omission (D9). **Both decided 2026-10-14**
  (tickets 33 and 34, option 1 each): in-flight statuses now block until
  Done/Merged, and Claude's result usage is populated to match Pi and Codex.
- The Worker cwd report (W1) is **not reproducible** against `main` or
  `v0.52.1`: jjfx sets the Run cwd correctly (guard test added) and opencode
  honours it, including in a colocated worktree. Ticket 05 is `needs-info` with
  a recurrence diagnostic; the leading untested variable is the reporter's
  `PATH` (`opencode` may be a wrapper).

## Fog

- Whether the Worker cwd failure is jjfx or opencode's project-root detection
  under colocated Git worktrees - **answered: neither, in isolation**; ticket 01
  could not reproduce it. Awaiting reporter info (ticket 05).
- Whether D8 (dependency statuses) and D9 (Claude usage) are intended
  simplifications or oversights.
- The exact liveness contract for a Dispatch Group that contains a terminal
  failure: cancel, skip-with-reason, or fail-the-group (ticket 04 decides).
- How much of the jj read path can move to `jj-lib` without breaking the
  version coupling (ticket 21 / 24).
