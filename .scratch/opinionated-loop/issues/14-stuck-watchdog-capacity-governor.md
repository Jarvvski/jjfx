# Stuck-agent watchdog and a dispatch capacity governor

Status: resolved
Type: task
Blocked by: none

## Problem

Two resilience gaps in the dispatch loop:

- **No stuck detection.** A Worker whose agent has wedged (waiting on a prompt it
  will never get, or looping) stays `busy` with no signal beyond elapsed time. The
  owner finds out by reading `jjfx status` and guessing.
- **No capacity governor.** `pool N` and a large `dispatch --all` fan out
  immediately, which can trip provider rate limits (a form of this is why ticket
  03 adds a quota-blocked state).

Gas Town's Refinery/Witness/`max_polecats` is the reference model: a supervisor
that notices wedged agents and a scheduler that keeps concurrency under a limit.

## Solution

- **Watchdog**: flag a Run with no progress (no new run-log activity, no state
  transition) within a configurable window as `stuck`, surfaced in the list.
- **Governor**: an optional cap on concurrent dispatch, so a batch is paced under
  the provider's limit rather than fired at once.

## Commits

1. Add stuck detection from run-log activity + elapsed time, surfaced as a
   distinct state (and folded into Attention as needs-you).
2. Add an optional concurrent-dispatch cap to the dispatch path, with a clear
   waiting state for tickets held back.
3. Tests; docs (`docs/configuration.md`); changelog.

## Acceptance Criteria

- [x] A Run with no progress within the window is surfaced as stuck.
- [x] A configured concurrency cap holds excess tickets in a visible waiting
      state rather than firing them.
- [x] With no cap configured, behaviour is exactly today's.
- [x] `mise run check` is green.

## Out of Scope

- The quota-blocked lifecycle state — ticket 03.
- Automatic re-dispatch of a stuck Run (surface only; the human decides).

## References

- `src/engine/pool.rs` (`reconcile_worker`), `src/render/pool.rs`
- Gas Town (watchdog + scheduler) as prior art

## Comments

- 2026-10-15: Filed from the strategy review.
- 2026-10-15: Resolved. **Watchdog** (`src/engine/watchdog.rs`): stuck is derived,
  not persisted - like occupancy (ADR-0011) it is a view of live Run state, so
  the Go-compatible Pool and Worker documents are untouched. The rule is a pure
  function over `RunProgress { status, started_at, log_activity_at, process_alive }`,
  and the frontend supplies the clock and the window. Progress is
  `max(started_at, log mtime)`: a Run that just started has nothing to be stuck
  on, a log still growing is progress by definition, and a Run whose process is
  gone is reconciliation's problem (it becomes failed, never stuck). Surfaced in
  three places: the home list folds it into `needs you`, the Pool view marks the
  Worker `[stuck]` beside its persisted `busy`, and `jjfx status` prints how long
  the Run has been quiet and counts it in the summary.
- 2026-10-15: **Decision - the watchdog is on by default (30 minutes),
  `stuck_after_secs = 0` disables it.** AC3 names the *cap*, and AC1 implies a
  window exists for a default install; a watchdog nobody enables is dead code.
  The default is deliberately conservative (a slow model turn is not a wedge).
  The one behaviour change with nothing configured is that a Run genuinely quiet
  for 30 minutes now reads as stuck - which is the ticket's whole point. Flip it
  off with `[watchdog] stuck_after_secs = 0` if that is unwanted.
- 2026-10-15: **Governor** (`src/engine/governor.rs`): `[dispatch] max_concurrent`
  is a ceiling on *concurrent Runs*, not a batch size - it counts the Workers the
  Pool already records as busy - so repeated Dispatches stay under the same limit
  rather than each firing a fresh batch. The split is applied in
  `DirectDispatch` (the one seam both the CLI and the TUI dispatch through) to
  the three batch entry points; `dispatch_reserved`, orchestration's single-ticket
  handoff, is left alone because orchestration already paces itself by waves and
  Pool capacity. Held Tickets are not reserved and not launched: they return as
  `DirectDispatchOutcome::Waiting` in input order, are named by the CLI and the
  Pool view, and are recorded in jjfx-owned `.jj/pool/waiting.json` (rewritten by
  every Dispatch, including an uncapped one, which clears it) so the backlog is
  visible after the Dispatch that held it - the same jjfx-owned-sidecar pattern
  as the occupancy marker.
- 2026-10-15: With no `max_concurrent` configured nothing changes: no split, no
  waiting record, and the existing Dispatch tests pass untouched. `dispatch`'s
  `partial` flag keeps its meaning (an explicit use-available Dispatch that found
  no capacity); a held-back Ticket is a different answer and does not set it.
- 2026-10-15: Config plumbing: `App::new` now takes one `config::DispatchSettings`
  (tickets + spend + dispatch + watchdog) instead of separate `tickets`/`spend`,
  because it was already at clippy's 7-argument limit and the cap must reach the
  TUI's Dispatch adapter. The CLI reads `[dispatch]` and `[watchdog]` directly
  where it needs them, following `review_command`'s precedent for the same
  argument-limit reason.
- 2026-10-15: Tests: `governor::pace` unit tests (cap, in-flight accounting, order),
  watchdog unit tests (window, progress, dead process, no evidence), a config test
  per new section, a `waiting.json` round-trip that surfaces through the snapshot,
  a render test for `[stuck]`, a `workspace_list` test that a stuck Run groups
  with needs-you, and two `tests/direct_dispatch.rs` cases: a full pool holding a
  whole batch with no Runtime at all, and a one-slot pool firing its first Ticket
  and holding its second against a fake Runtime.
