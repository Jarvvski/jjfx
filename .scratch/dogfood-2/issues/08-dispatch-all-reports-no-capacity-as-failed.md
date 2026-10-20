# `dispatch --all` reports a capacity gap as `failed`, and the cap detail says "0 in flight"

Status: resolved
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

- [x] A Ticket with no free Worker is reported the same way by `dispatch --all`
      and by a named `dispatch`, as a capacity gap (`waiting`), not `failed`.
- [x] A held-back-by-cap detail states the in-flight count that actually bound
      the batch (the count the `governor` object reports), not a pre-launch zero.

## References

- `.scratch/harness/issues/05-capacity-governor.md`
- `src/engine/governor.rs`, `src/engine/orchestration.rs`, `src/cli.rs`
  (`dispatch --all` outcome construction)

## Triage (2026-10-20)

Triaged from `needs-triage` to `ready-for-agent`. The evidence is a **live
reproduction during the second drive**, not a code reading: the drive hit this
while working a real backlog through jjfx, and the report
(`.scratch/dogfood-2/report.md`) records what happened around it.

Not independently re-verified - the drive reproduced it live.

The acceptance criteria are unchanged - they were already specific and
testable, which is why this needed triage rather than a rewrite.

## Resolution

The Worker Pool is the other dial that bounds Dispatch, and a Pool with no idle
Worker is a capacity gap, not a failure - the Ticket is *waiting* for a Run to
finish, exactly as one the concurrency cap holds back is. The two ways to reach
the gap now say the same thing:

- One shared reason, `POOL_CAPACITY_GAP_DETAIL`, is carried by both paths: the
  `--all` path (`src/engine/orchestration.rs`) and the named path
  (`src/engine/direct_dispatch.rs`), which previously turned the gap into a
  `Failed` outcome in phase `Capacity`.
- A named `dispatch` whose reservation finds no idle Worker now returns
  `OrchestrationStart::Waiting { ticket, detail }` instead of a hard
  `Execution` error, so `src/cli.rs` renders it as `DispatchEntry::Waiting` -
  the same shape `--all` already used. The removed `Capacity` failure phase and
  its now-dead branches are gone.
- `cap_hold_detail(cap, in_flight, fired)` names `in_flight + fired`: the count
  that actually bound the batch, and the count `jjfx status --json`'s
  `governor.running` reports once the batch has fired. The pre-launch count read
  `0` for a first Dispatch even though the cap was exactly what held it.

The TUI consumes `OrchestrationStart` exhaustively, so the new `Waiting` start is
forwarded and shown as a status (`src/workspace_dispatch.rs`, its `adapter.rs`,
`src/pool_session.rs`).

Verified: `mise run check` (changelog validation + fmt + clippy -D warnings +
build + test) passes.
`a_pool_with_no_free_worker_is_reported_as_waiting_by_all_and_by_a_named_dispatch`
asserts both paths report the same Ticket as `waiting` with the same detail, and
`a_capped_dispatch_names_the_count_that_bound_the_batch` asserts the detail names
`2 Run(s) in flight` and not the pre-launch `0`.
