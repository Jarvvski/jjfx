# Miscellaneous low-severity fixes

Status: needs-triage
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

- [ ] Each confirmed defect has a failing test then a fix.
- [ ] Each behaviour question is answered in `## Comments` before code changes.
- [ ] `mise run check` is green.

## Out of Scope

- Anything already covered by another ticket in this effort.
