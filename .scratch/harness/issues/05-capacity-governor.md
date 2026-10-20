# Enforce dispatch capacity deterministically

Status: resolved
Type: task
Blocked by: none

## Problem

Concurrency is bounded by an instruction, not by the tool. A root session told
"don't work on more than 3 bugs at a time" was measured at **6 concurrent**
children.

jjfx already has a pool size and a held-back set (`waiting.json`) for the last
dispatch, so the mechanism is half there - what is missing is a durable,
enforced limit that survives across dispatches and is visible.

## Acceptance Criteria

- [x] A configured concurrency limit is enforced across dispatches, not just
      within one.
- [x] A dispatch that would exceed it holds work back, and says what it held
      back and why.
- [x] The limit is visible in `status --json` (how many are running, how many
      are held, what the limit is).
- [x] A test drives more ready work than the limit and asserts the limit holds.

## References

- `src/engine/eligibility.rs`, `src/engine/pool.rs`
- `docs/adr/0012-named-worker-pools.md`

## Resolution

The governor already existed as a batch pacer; the ticket closed the three gaps
that made it an *instruction* rather than a *tool-enforced* limit.

**The limit and how it is enforced.** `[dispatch] max_concurrent` (user config;
`src/config.rs::DispatchConfig::cap`) is read into an engine `ConcurrencyCap` at
the two places a whole batch is dispatched - `cli::dispatch_with_capacity_prompt`
and the TUI's dispatch adapter. Enforcement is *across dispatches*: the batch is
split by `governor::pace(cap, in_flight, requests)`, and `in_flight` is the count
of Workers the Pool records as **busy**, not the size of the batch in hand. A
second Dispatch that arrives while the first Dispatch's Runs are still running
therefore sees no room and holds its whole ready set, instead of each Dispatch
firing a fresh `limit`.

**The gap closed.** The orchestration wave (`jjfx dispatch <PARENT>`) was bounded
only by idle Workers, so a Parent's Sub-issues could exceed the cap even though a
`dispatch --all` batch could not. `OrchestrationRequest` now carries the same
cap, `run_with_execution` counts in-flight busy Workers (plus the Runs it
launches, minus the ones it resets), and holds a ready Sub-issue back with the
existing `WaitingForCapacity` event once the ceiling is reached.

**What a held-back Dispatch reports.** Each held Ticket carries a reason, not
just a count:
`held back by the concurrency cap (dispatch.max_concurrent = 2): 2 Run(s) in
flight, so a Run must finish before another may start`. The same detail is
written to the durable work queue, so a queued step records why it did not run.

**Visibility.** `status` and `status --json` report the governor. JSON gains a
top-level `governor` object - `{"limit": N|null, "running": N, "held": N}` - and
the prose prints `Concurrency: N Run(s) in flight (dispatch.max_concurrent = N),
M held back`. `running` is every busy Worker across every pool; `held` is the
`waiting` set; `limit` is the configured cap, `null` when uncapped.

**Governor versus Pool capacity (the decision).** They are two dials, not one.
A Pool's **size** is a *capacity declaration* - how many Workers exist, so how
many Runs can physically be held (`jjfx pool N`). The **governor** is a *ceiling
on how much is in flight* - a property of the Repository's dispatch settings,
sized to pace a burst rather than to size a lane. Both bind and the smaller wins:
a pool of ten under a cap of three still fires three. They are reported
separately - the cap in the top-level `governor`, the pool's capacity as `size`
on its `pools` entry - and this relationship is stated in the
`src/engine/governor.rs` module docs.

**Verification.** `tests/capacity_governor.rs` drives the real binary over more
ready work than the limit: a four-Worker Pool with two Runs already in flight and
three ready Tickets under a cap of two. It asserts all three are held (the two
free Workers untouched - the governor, not capacity, is binding), that
`status --json` reports `limit 2 / running 2 / held 3` (and `null` when
uncapped), and that a second `dispatch --all` holds the same work - the
cross-dispatch claim. `src/engine/orchestration.rs` adds two unit tests for the
wave cap (prefix fires, rest wait; a wave at the ceiling fires nothing), and the
existing `tests/direct_dispatch.rs` cap tests assert the new held-back reason.
`mise run check` passes (changelog validation, fmt, clippy warnings-as-errors,
build, full test suite).
