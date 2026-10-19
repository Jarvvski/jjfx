# Helper-subprocess deadlines in the test suite are load-sensitive

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

The integration suite waits on helper subprocesses with fixed wall-clock
deadlines, and those helpers do real `jj`/`git` work whose duration depends on
machine load. Under load the work legitimately exceeds the budget and the test
fails, even though nothing is wrong:

- `tests/worker_pool.rs:235` - `Duration::from_secs(10)` for the
  `stale_reservation_after_destroy_helper` subprocess, which runs
  `local_repository_with_origin()` + `pool.resize_to(1)` (a `jj workspace add`) +
  `destroy()` (jj operations) + `run_reserved_background` + a second `destroy`.
- 17 further fixed deadlines of 1s/3s/5s across `worker_pool.rs`,
  `worker_actions.rs`, `state_processes.rs`, `agent_runtime.rs`,
  `direct_dispatch.rs`, `ticket_discovery.rs`, and `unix_primitives.rs`
  (which already has a named `HELPER_TIMEOUT`), guarding **59**
  `current_exe()` helper spawns.

`mise run check` must be green, so this is a **verification-reliability** defect:
a red gate that isn't a real regression costs a re-run at best and a wrong
conclusion at worst.

## Evidence

Hit independently by **three** agents in one session, all on
`worker_pool::stale_reservation_cannot_persist_a_pid_after_destroy_detaches_membership`
("stale Reservation helper timed out"), while the machine ran concurrent cargo
builds. Measured by one agent, interleaving base-revision and patched test
binaries in the same alternating loop:

```
base    mean 7.9s / max 26.2s
patched mean 4.9s / max  6.8s
```

and the helper burned only ~1.3s of CPU while taking 7-48s of wall clock - it is
**pure wait** on subprocesses, not a regression. The same agent confirmed the
helper passes standalone, and that the base revision shows the same latency, so
this is environmental rather than caused by any recent change.

Do not go hunting for a product bug behind this: it is the deadline, not the
code under test.

## Fix

A fixed wall-clock budget for load-dependent work is inherently flaky. Options:

1. **One shared, generous deadline.** Promote `unix_primitives.rs`'s
   `HELPER_TIMEOUT` idea into a single test-support constant (e.g. 60s) and use it
   at every site, so the budget reflects "this is a liveness guard against a
   hang", not "this is how long the work should take". Smallest change; a genuine
   hang still fails, just later.
2. **Progress-based liveness.** Have the helper touch a phase marker as it moves
   through its steps, and fail only when there has been *no progress* for N
   seconds. Robust under arbitrary load and still fails fast on a real hang;
   costs more plumbing across 59 spawn sites, so it may be worth applying only to
   the helpers that do real repository work.
3. **Both** - a shared generous budget now, progress-based liveness for the
   expensive helpers.

Prefer 1 if the goal is simply to stop the flakes; prefer 2 (or 3) if the suite
is expected to run on loaded CI where a 60s budget may still not be enough.

Whatever is chosen, do not just bump the single 10s literal: the same class is at
every site listed above, and `worker_pool.rs:108` (5s) guards the sibling
`destroy_live_process_helper`.

## Acceptance Criteria

- [ ] The suite is green under the same concurrent load that produced the flake
      (verify by running the process/timing tests while a build is in flight, and
      repeat them enough times to be convincing).
- [ ] A genuinely hung helper still fails the test within a bounded time - the
      change must not turn a liveness guard into no guard.
- [ ] The chosen budget/mechanism is documented where it is defined, with the
      reason (load-dependent subprocess work), so it is not "tidied" back down.
- [ ] `mise run check` is green.

## Out of Scope

- Product behaviour. This ticket changes test infrastructure only; the evidence
  above shows no product defect behind the timeouts.

## Outcome (2026-10-13)

**Resolved.** `24dcce` - `Share one liveness budget for helper-subprocess
deadlines in tests`.

Option 1: one `HELPER_LIVENESS_TIMEOUT = 120s` in the new `tests/support/mod.rs`,
documented as a guard against a *hang* rather than a statement of expected
duration, with the 1.3s-CPU-vs-7-48s-wall measurement inline so a future reader
does not "tidy" it back down. Progress-based liveness (option 2) was rejected as
plumbing across ~59 spawn sites for a benefit this load does not demand.

Evidence: the named flaking test 12/12 green; direct measurement of its helper
showed **10 of 12 runs exceeded the old 10s deadline**, so the old budget would
have failed 10/12 - the budget was the defect, not the code. Forcing the constant
to 1s fails the test in 1.02s, so the guard is still live rather than merely
removed.

Deliberately left alone: `ticket_discovery.rs`'s `TIMEOUT`/`BUDGET` are
*behavioural assertions* (the product timeout under test, sized against a 30s
sleeping helper), not liveness guards - raising them would have broken the
`expect_err` the test exists to make.

No changelog entry: test infrastructure, nothing user-visible.

## Re-opened Mon 19 Oct 2026

Reconciled against the merged code at `9d6bd0ce6e7b`. Criterion 1 does **not**
hold, so this ticket is not done and is back to `ready-for-agent`. All boxes are
left unticked rather than partially ticked.

The fix itself shipped and is real. `tests/support/mod.rs:71` defines the shared
`HELPER_LIVENESS_TIMEOUT = 120s`, documented at `:55-70` as a guard against a
hang rather than a statement of expected duration, and it is used at the
helper-spawn sites in `direct_dispatch.rs`, `unix_primitives.rs`, `worker_pool.rs`
and `agent_runtime.rs`. Criterion 3 (the budget is documented, with the reason)
holds, and criterion 2 (a genuine hang still fails within the bound) is supported
by the ticket's own forced-1s measurement above.

What does not hold is the class. The same load-sensitive-deadline failure
recurred during the named-pools work in a test this ticket never reached:
`engine::locks::tests::a_blocked_acquisition_does_not_hold_the_pool_lock_while_it_waits`
(`src/engine/locks.rs:157`) flaked **three times** under load. It is not a helper
subprocess - it spawns a `thread` and polls a fixed one-second window
(`src/engine/locks.rs:181`) behind its own `const LIVENESS: Duration =
Duration::from_secs(30)` (`src/engine/locks.rs:115`) - so the shared
`HELPER_LIVENESS_TIMEOUT` does not reach it and a load-sensitive wall-clock
budget for load-dependent work, the exact defect this ticket names, is still
present there.

To close it: give the locks test the same treatment - a budget that reflects a
liveness guard against a hang rather than an expected schedule, or progress-based
liveness - so a loaded machine cannot red it. Do not simply re-run it.
