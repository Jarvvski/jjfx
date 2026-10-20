# `dispatch --all` reports a capacity gap as `failed`, and the cap detail says "0 in flight"

Status: needs-triage
Type: bug
Blocked by: none

## Problem

Two reporting defects in the Dispatch outcome, both seen while driving the
fixture's backlog.

**1. No free Worker is reported as `failed`, not `waiting`.** The skill's contract
is that a Ticket with no free Worker "reports the capacity gap rather than
waiting silently" (`skills/jjfx-dispatch/SKILL.md`). But `--all` marks it `failed`
with a null Worker and no detail, while an explicit Dispatch of the same Ticket
is a hard error:

    $ jjfx dispatch --all --json
    [{"ticket":"sift--05-run-summary","outcome":"failed","worker":null,
      "phase":null,"pid":null,"detail":null}, ...]

    $ jjfx dispatch --json sift--05-run-summary
    Error: orchestration execution failed: no idle Worker is available for Ticket
    sift--05-run-summary (available: 0)

`failed` reads as "the work broke", not "there was no room", and it is not the
same shape a `waiting` outcome carries.

**2. The concurrency-cap detail names the wrong number.** A Ticket held back by
the governor reports the in-flight count *before* the batch's own launches, so it
says "0 Run(s) in flight" when the cap is exactly what held it:

    $ jjfx status --json | jq .governor
    {"limit":2,"running":2,"held":4}
    # the held items' detail, from the same batch:
    "held back by the concurrency cap (dispatch.max_concurrent = 2): 0 Run(s) in
     flight, so a Run must finish before another may start"

A second Dispatch in the same window reports `2 Run(s) in flight` for the same
situation, so the two messages disagree about the same state.

## Acceptance Criteria

- [ ] A Ticket with no free Worker is reported the same way by `dispatch --all`
      and by a named `dispatch`, as a capacity gap (`waiting`), not `failed`.
- [ ] A held-back-by-cap detail states the in-flight count that actually bound
      the batch (the count the `governor` object reports), not a pre-launch zero.

## References

- `.scratch/harness/issues/05-capacity-governor.md`
- `src/engine/governor.rs`, `src/engine/orchestration.rs`, `src/cli.rs`
  (`dispatch --all` outcome construction)
