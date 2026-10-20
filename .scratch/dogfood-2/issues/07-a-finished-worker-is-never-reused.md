# A finished Worker is never returned to the pool, so a pool of N does N tickets

Status: needs-triage
Type: bug
Blocked by: none

## Problem

A Worker that finished a Run reads `done`, not `idle`, and nothing returns it to
the pool. A later Dispatch therefore sees no capacity even though the Worker is
sitting there with its Workspace intact, and the pool silently serves only as
many Tickets as it has Workers, ever.

Observed on a four-Worker pool after four Runs:

    $ jjfx dispatch --all --json
    sift--04-count-by-field -> dispatched worker-ccb8a5
    sift--05-run-summary    -> failed
    sift--06-relative-time  -> waiting
    sift--07-auto-buckets   -> waiting
    # stderr:
    Pool has 1 idle worker(s) but 2 ticket(s) to dispatch. Grow pool by 1? Not a
    terminal, so not growing the pool; run `jjfx pool <N>` to grow it.

and a named Dispatch is a hard error:

    $ jjfx dispatch --json sift--05-run-summary
    Error: orchestration execution failed: no idle Worker is available for Ticket
    sift--05-run-summary (available: 0)

The remedy - `jjfx pool reset <worker>` - works, and is the only thing that
reclaims a finished Worker:

    $ jjfx pool reset worker-1be12c
    Reset worker-1be12c to idle

But nothing points at it: `dispatch` recommends *growing the pool*, and the
skill's flow (dispatch → wait → land) never mentions resetting. An agent working
a backlog larger than the pool size stalls until it discovers `pool reset` on its
own.

## Acceptance Criteria

- [ ] A finished Worker is available to a later Dispatch (returned to `idle`
      automatically once its Run's outcome is collected), or the flow that
      reclaims it is part of the documented dispatch loop and is what a
      no-capacity Dispatch names - not "grow the pool".
- [ ] `dispatch` and `dispatch --all` report the same thing when the pool has no
      free Worker (see the separate finding on `failed` vs `waiting`).
- [ ] A test dispatches more Tickets than the pool size across successive rounds
      and asserts every Ticket runs without a manual `pool reset`.

## References

- `src/engine/pool.rs` (`WorkerStatus`, reset), `src/engine/eligibility.rs`
- `skills/jjfx-dispatch/SKILL.md` (steps 1, 7)
