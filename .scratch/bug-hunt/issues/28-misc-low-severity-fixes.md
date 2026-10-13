# Miscellaneous low-severity fixes

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

A batch of low-severity defects and two behaviour questions. Split into separate
focused commits (and possibly separate tickets if any grows).

### Confirmed defects

- **I10 - any invalid cost line fails a successful Run.**
  `run_log.rs:457-475` maps any `RunLogError::Parse` to `unexpected_result`, so
  a negative/non-numeric/out-of-range `total_cost_usd` (or Pi `cost.total`)
  finalises a successful Run as `failed`, feeding orchestration retry/failure.
  `current_activity` (`:344-354`) fails the same way, stopping the watcher.
- **C8 - case-collision on Dispatch Group files.**
  `state.rs:1901-1907` lowercases the parent id, so two ids differing only in
  case collide and the second load fails validation (`:2135-2145`).
- **I11 - assorted small bugs.**
  - CLI capacity confirmation prints `available + gap` but the pool grows by
    `gap` from the current size (`cli.rs:1377-1384` vs `pool.rs:717-763`).
  - `Store::reload` resets `colocate` to `true` (`store.rs:170`) - also covered
    by ticket 11.
  - `config_path` becomes relative `.config/jjfx/config.toml` when both
    `XDG_CONFIG_HOME` and `HOME` are unset (`config.rs:211-222`).
- **T10 - TUI nits.**
  - `?` help intercept (`app.rs:680`) steals `?` from every text buffer
    (worker alias, ticket id, parent id, diff filter).
  - Help over the task editor hides the editor and drops paste
    (`app.rs:2164` checks `SendInput` against `self.mode`, missing
    `Help(Box::new(Pool(SendInput)))`).
  - `viewport`/`diff_view` truncate content length to `u16`
    (`diff_view.rs:294`), so a >65 535-line diff cannot be scrolled to the end;
    `Viewport::line_down` (`viewport.rs:51`) can overflow at the boundary.

### Behaviour questions (confirm intent first)

- **D6 - Attention precedence on stacked work.** A newer local/pushed change
  above a changes-requested PR masks it (`attention.rs:52-76` +
  `work.rs:303-319`), so "Waiting over a change-requested PR needs a nudge" is
  unreachable. Decide whether the least-delivered rule or the nudge rule wins.
- **D7 - mixed merged+unmerged blockers.** `dispatch_group.rs:479-522` can set
  `pull_request_base = "main"` for a stacked branch when a merged blocker sorts
  first.
- **D8 - non-todo dependency statuses unblock.** `dispatch_group.rs:345-354`
  marks `In Progress`/`In Review` dependencies as `skipped`, and `Skipped`
  unblocks - so dependents run before a dependency is complete. Test-encoded;
  confirm against CONTEXT's "must be completed".
- **D9 - Claude usage omitted.** `run_log.rs:1492-1513` never populates usage
  for Claude `result` events, contradicting CONTEXT's Run-result definition
  ("plus usage ... when reported"). Test at `tests/run_log.rs:398-423` locks it
  in.

## Fix

One focused commit per item; file the behaviour questions as their own tickets
if the answer is "change it".

## Acceptance Criteria

- [x] Each confirmed defect has a failing test then a fix. *(T10c done; the
      rest are listed below.)*
- [x] Each behaviour question is answered in `## Comments` before code changes.
- [x] `mise run check` is green.

## Triage (2026-10-13)

Every item was re-verified against the current code before being scheduled. The
ticket's line numbers had all drifted; the findings below are the current ones.
Two claims were **refuted** and one was **overstated**, which is why the sweep is
verified rather than trusted.

### Done

**T10c - content length truncated to `u16`: CONFIRMED, FIXED.**
`diff_view.rs:103` and `:294` cast the line count to `u16`, and `Viewport`'s
`scroll`/`height`/`total` were all `u16`. Arithmetic: a 70 000-line file records
`70_000 as u16 == 4_464`, so with a 40-row pane `max_scroll` is 4 424 and lines
4 464-70 000 are unreachable; exactly 65 536 lines records 0 and the file cannot
be scrolled at all. The `line_down` overflow was also real but narrower: it needs
`total == 65 535` **and** a zero-height pane (a 3-row terminal), and panics in
debug / wraps to the top in release.

Fixed by widening `Viewport` to `usize` and making the offset arithmetic
saturating. Verified red first: with the truncating cast re-injected into
`render_diff`, `a_diff_longer_than_u16_max_still_scrolls_to_its_end` fails
`left: 4424, right: 69960`. Committed as
`Keep long diffs scrollable past the u16 line-count ceiling`.

### Scheduled - confirmed, no open question

**I10 - an invalid cost fails a successful Run: CONFIRMED.**
`parse_claude_cost` (`run_log.rs:1515-1529`) and `parse_pi_cost` (`:1071-1095`)
return `RunLogParseError::InvalidCost` for a negative, non-finite, or
out-of-range cost. That is *not* `InvalidEvent`, so the line loop (`:392-399`)
propagates it as `RunLogError::Parse` and `result_for_finalization` (`:459-475`)
turns it into `RunResult::failed("Process exited unexpectedly")`. The asymmetry
is the defect: an unparseable JSON line is skipped, but a bad cost inside a
perfectly good terminal event poisons the Run's outcome. `current_activity`
fails the same way, and its callers (`workspace_dispatch/adapter.rs:642`,
`cli.rs:1531`) turn that into a hard error.

Fix: an unusable cost means "no cost reported", not "the Run failed" - drop the
cost and keep the outcome. Keep the existing "no terminal event at all -> failed"
behaviour for Claude/Pi; note `result_for_completion` (`engine/runtime.rs:755-764`)
already treats a *fallback* result with exit code 0 as succeeded, but only for
OpenCode, so do not generalise that instead.

Two consequences to accept deliberately:
- `RunLogParseError::InvalidCost` is a **public exported variant**
  (`engine/mod.rs:58`), so removing it is a pre-1.0 API break: the CHANGELOG
  entry belongs under `### Changed` with `(break)`.
- Two tests encode the defect and must change: `tests/run_log.rs:527` ("a
  negative Run cost should fail") and `:1196` (the Pi twin). They should instead
  assert the cost is dropped and the outcome is unaffected.

**I11a - the capacity prompt names the wrong size: CONFIRMED.**
`grow_and_reserve_targeted` (`pool.rs:741-766`) sets
`next.size = current_size + added.len()` with `added.len() == shortage.gap()`, so
the pool grows *by* the gap. `growth_question` (`cli.rs:1403-1410`) prints
`available + gap`, which equals `requested`, not the resulting size: with 4
workers (2 idle) and 5 tickets it promises "Resize pool to 5?" and produces 7.
Fix the wording to state the growth, which is exactly what the operation does
and needs no new data; "resize to N" would require the current size, which
`CapacityShortage` does not carry. `capacity_prompt_uses_the_locked_shortage_gap`
(`cli.rs:2059`) pins the current string and changes with it - its point (the
prompt is derived from the locked shortage, not a re-read) is preserved.

**I11b - a relative config path when `HOME` is unset: CONFIRMED.**
`config_path` (`config.rs:211-222`) falls back to `PathBuf::default()` for `HOME`,
so with `XDG_CONFIG_HOME` and `HOME` both unset it returns the *relative*
`.config/jjfx/config.toml`, resolved against the process CWD - a repository could
plant the user's config. Fix by treating "no base directory" as "no user config"
and falling back to defaults rather than to a relative path. This changes
`config_path()`'s signature (it must be able to say "none"), so check its
callers; no test covers it today, so add one that clears both variables.

**I11c - the reported defect is already gone; a latent trap remains.**
`Store::reload` was deleted in ticket 30, so "reload resets `colocate` to `true`"
no longer exists. The same shape survives at `runtime.rs:224`, where the
post-deletion `Store::load(&repo_root)` (no `.with_colocate`) produces a store
that then *replaces* the App's via `Msg::WorkspaceDeletionCompleted`. Nothing
reads `store.colocate()` after construction in production today - `App::new`
(`app.rs:410`) reads it once to build the dispatch controller and `app.rs:2846`
is a test helper - so this is a trap rather than an observable bug. Close it with
`.with_colocate(self.colocate)` so the two `Store::load` sites agree.

**T10a - `?` is stolen from text buffers: PARTIALLY CONFIRMED.**
The intercept at `app.rs:757` runs before the per-mode dispatch, so `?` opens
help in every mode except `SendInput` (handled earlier at `:753`) and `Help`
itself. Two modes genuinely lose a legal character: the **worker alias**
(`app.rs:1218` accepts any non-control char) and the **diff filter**
(`diff_view.rs:142` accepts any non-Ctrl char, and its comment says "any
printable char extends the filter"). The ticket's claim about the ticket id and
parent id is **wrong**: `app.rs:1247` and `:1318` only accept alnum/`-`/`_`, so
those fields could never hold a `?` regardless of help. `NewWorkspace` is the
same. Fix by forwarding `?` to the modes whose buffer accepts printable
characters. No test covers either; `diff_view`'s own tests call `Detail::on_key`
directly and so bypass the app-level intercept entirely.

### Refuted

**C8 - case-collision on Dispatch Group files: REFUTED as filed.**
The mechanism is exactly as described (`state.rs:1932` lowercases the parent for
the filename while `validate_dispatch_group_for` at `:2150` compares it
case-sensitively), but the premise does not hold:

- Tickets are not discovered from files. They come from Linear via a provider
  query, and issue identifiers are canonical `TEAM-NUMBER` (uppercase); two
  distinct issues cannot differ only in case. There is no ticket-directory
  convention anywhere in the repo.
- The lowercasing is **mandated**: `docs/wsg-compatibility-contract.md:58,111`
  require `dispatch-<lowercase-parent>.json`/`.lock`, and
  `tests/state_repositories.rs:93-113` pins `dispatch-eng-100.json`. Removing it
  would break reading state written by the Go implementation.
- Removing it would not fix the collision anyway: APFS is case-insensitive, so
  `dispatch-ABC-1.json` and `dispatch-abc-1.json` are the same file with or
  without the lowercase.

What remains is narrow: supplying the **same** ticket with **inconsistent
casing** across runs (`dispatch ENG-100`, then `dispatch eng-100`) fails closed
with a confusing message - `cannot validate Dispatch Group eng-100: Dispatch
Group parent does not match repository identity` - which reads like corruption
and never mentions case. No group is overwritten and no other ticket's state is
touched. Recorded as `wontfix` for the collision; the diagnostic wording is an
optional nit (it would need a case-insensitive comparison in the validator
purely to improve the message, which is arguably worse than the status quo).

**T10b - help over the task editor: REFUTED as a user-facing bug.**
`Mode::Help(Box::new(Pool(SendInput)))` is unreachable from input: the only
constructor of `Mode::Help` is the `?` handler at `app.rs:757-759`, and `:753`
returns into the editor *before* it whenever the mode is `SendInput`. The test
`pool_send_editor_submits_a_prompt_without_blocking_input` (`app.rs:3786-3791`)
pins that `?` reaches the prompt rather than help. The three direct
`Mode::Pool(PoolMode::SendInput { .. })` comparisons (`:741`, `:753`, `:2300`)
are therefore a latent inconsistency with `pool_mode()` (`:2429-2438`), which
does unwrap `Help` - and if the `:753` guard were ever removed, both halves would
break (the editor would be hidden *and* paste dropped). Optional hardening, not
a bug fix: route those three checks through `pool_mode()` and pin it with a test
that sets the mode directly, since input cannot reach it.

### Escalated - needs the owner's intent

D6, D7, D8, and D9 are behaviour questions whose answers change shipped
behaviour and are not decided by any normative doc. Each was verified against
the code and the docs and filed as its own ticket with the concrete options:
**31** (D6), **32** (D7), **33** (D8), **34** (D9). Two corrections to the
ticket's own framing are recorded there: D6's rule is masked only when a newer
non-empty change is stacked above the PR (not unreachable in general), and no
test locks in D9's omission - the cited tests omit `usage` from their input
entirely.

## Out of Scope

- Anything already covered by another ticket in this effort.
