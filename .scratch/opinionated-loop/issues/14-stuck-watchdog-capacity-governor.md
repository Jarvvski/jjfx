# Stuck-agent watchdog and a dispatch capacity governor

Status: ready-for-agent
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

- [ ] A Run with no progress within the window is surfaced as stuck.
- [ ] A configured concurrency cap holds excess tickets in a visible waiting
      state rather than firing them.
- [ ] With no cap configured, behaviour is exactly today's.
- [ ] `mise run check` is green.

## Out of Scope

- The quota-blocked lifecycle state — ticket 03.
- Automatic re-dispatch of a stuck Run (surface only; the human decides).

## References

- `src/engine/pool.rs` (`reconcile_worker`), `src/render/pool.rs`
- Gas Town (watchdog + scheduler) as prior art

## Comments

- 2026-10-15: Filed from the strategy review.
