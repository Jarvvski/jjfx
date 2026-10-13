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
| C1 | concurrency | H | S | `send`/`review`/`rebase` skip the workspace-op lock; Reset's async restore races a new Run | 03, 08 |
| C2 | process | H | S | PID reuse can TERM/KILL an unrelated process group on Reset/destroy | 07 |
| T1 | TUI | H | S | blocking kitty subprocess + ~5s sleep on the UI thread | 17 |
| T2 | TUI | H | S | help over Detail/Graph renders nothing yet swallows every key | 18 |
| I1 | forge | H | S | every `gh` failure is reinterpreted as "no origin remote" | 10 |
| C3 | state | M-H | S | `.cleanup` marker written before the pool manifest permanently blocks a Worker | 09 (fixed) |
| C4 | process | M | S | foreground launch failure kills only the direct child; foreground Runs aren't their own group | 20 |
| C5 | state | M | S | `detached_cleanup_markers` aborts on any malformed `.cleanup` filename | 20 |
| C6 | process | M | S | `cleanup_untracked_run` can release a reservation while the group is alive | 20 |
| D2 | dispatch | M | V | branch repair hardcodes `adam/` while dispatch derives the prefix from `user.name` | 14 |
| D4 | dispatch | M | S | orchestration freezes occupancy exclusions for the whole run (violates ADR-0011) | 15 |
| D5 | dispatch | M | S | retry path resets the Worker before persisting the group; a save conflict strands it | 16 |
| I2 | forge | M | S | PR read failures degrade to "no PRs"; `--limit 100` drops older PRs | 10 |
| I3 | forge | M | S | conflict detection only inspects `@`; a lower-commit conflict welds and pushes | 10 |
| I4 | forge | M | S | `pushed_something` sniffs the English string `"Nothing changed."`; a failed push reads as no-op | 10 |
| I5 | store | M | S | `default` workspace pinned to the current workspace when launched from a secondary workspace | 11 (fixed) |
| I8 | dispatch | M | S | untrusted Linear text injected verbatim into prompts | 12 |
| T3 | events | M | S | one invalid UTF-8 byte wipes all startup agent state | 19 |
| T4 | events | M | S | replay/tailer offset race drops events | 19 |
| T5 | events | M | S | rotation only at startup; unbounded `events.jsonl` growth + rotation race | 19 |
| T6 | TUI | M | S | blocking `jj tidy`/`tidyws` on the UI thread | 17 |
| T7 | TUI | M | S | per-tick `canonicalize` and duplicate graph/row projection per frame | 26 |
| I6 | work | M-L | S | workspace names interpolated unquoted into revsets | 22 |
| I7 | workspace | M-L | S | two divergent ad-hoc workspace path rules (CLI vs TUI) | 23 |
| I9 | dispatch | M-L | S | duplicate Ticket ids are never deduped -> double dispatch | 13 |
| I10 | run log | L-M | S | any invalid cost line turns a successful Run into a failure | 28 |
| C7 | process | L-M | S | a recycled PID keeps a Worker `busy` forever | 07 |
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

### Phase 1 - high severity

- [04](issues/04-terminate-group-on-failed-dependency.md) - D1.
  **resolved** - quiescence + `blocked` count.
- [05](issues/05-fix-worker-run-working-directory.md) - W1 (blocked by 01).
  **needs-info** - no reproducible jjfx defect; recurrence diagnostic recorded.
- [06](issues/06-exclude-git-exported-bookmarks.md) - D3 (blocked by 02).
  **wontfix** - refuted.
- [07](issues/07-durable-run-identity.md) - C2, C7.
- [08](issues/08-serialize-workspace-operations.md) - C1 (blocked by 03).
- [09](issues/09-order-cleanup-marker-after-manifest.md) - C3.
  **resolved** - manifest-first ordering.

### Phase 2 - medium

- [10](issues/10-forge-failure-and-push-correctness.md) - I1, I2, I3, I4.
- [11](issues/11-normalize-repository-root.md) - I5 (blocked by 02).
  **resolved** - root normalised at discovery.
- [12](issues/12-fence-untrusted-ticket-content.md) - I8.
- [13](issues/13-deduplicate-dispatch-tickets.md) - I9.
- [14](issues/14-single-source-branch-prefix.md) - D2.
- [15](issues/15-live-occupancy-exclusions.md) - D4.
- [16](issues/16-persist-group-before-reset.md) - D5.
- [17](issues/17-move-blocking-work-off-ui-thread.md) - T1, T6.
- [18](issues/18-help-overlay-in-detail-and-graph.md) - T2.
- [19](issues/19-event-log-tolerance-and-tailing.md) - T3, T4, T5.
- [20](issues/20-workspace-cleanup-robustness.md) - C4, C5, C6.

### Phase 3 - improvements

- [21](issues/21-typed-jj-access-layer.md) - one jj seam with typed outcomes.
- [22](issues/22-quote-refs-and-validate-workspace-names.md) - I6.
- [23](issues/23-unify-ad-hoc-workspace-paths.md) - I7.
- [24](issues/24-jj-lib-jj-version-compat-check.md) - version coupling.
- [25](issues/25-consolidate-state-write-and-lock-helpers.md) - one atomic
  write/lock helper.
- [26](issues/26-tui-per-frame-work-and-agent-state-cache.md) - T7.
- [27](issues/27-terminal-restore-on-signal-and-input-resilience.md) - T8, T9.
- [28](issues/28-misc-low-severity-fixes.md) - I10, C8, D6, D7, D8, D9, I11, T10.

- The sweep finding D3 (git-exported bookmarks counted as "pushed") is
  **refuted**: `remote_bookmarks()` already excludes the colocated `git` remote.
  The sweep misread the `remote_bookmarks` template keyword as the revset
  result. Verified with `jj 0.46.0` on a colocated repo with an unpushed
  bookmark. This is why every sweep finding is tested before it is fixed.

## Decisions so far

- The `wsg` compatibility contract **explicitly excludes** parent-directory
  fsync (`docs/wsg-compatibility-contract.md:137`), so `write_atomic`'s missing
  directory fsync is not a defect to file. Ticket 25 may still add it as a
  documented improvement.
- PID-as-identity is acknowledged in the contract ("liveness hints, not Run
  identity", `:147`). The fix (ticket 07) is a durable Run token, **not** a
  change to the frozen Worker document.
- Two findings are test-encoded current behaviour - non-todo dependency statuses
  unblocking (D8), and Claude usage omission (D9). Confirm intent before
  changing; both live in ticket 28.
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
