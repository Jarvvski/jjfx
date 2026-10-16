# Process- and exec-timing tests are flaky under CI and load

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

Several integration tests that assert on process timing, or on an executable's
open-for-write state, fail intermittently - under a loaded machine and, confirmed
2026-10-15/16, in CI. They are **not** a code break: the same `mise run check`
passed on the pull request for each change below and failed on the *main* push
run for two of them, and the later main runs are green. But a suite that fails
one run in N makes "CI green" an unreliable signal, and it cost the
`opinionated-loop` batch repeated re-runs - the local workers hit it on nearly
every ticket.

## Evidence (2026-10-15/16)

Failures on **main** CI runs (post-merge), where the PR check for the same commit
had passed:

- `#16` main run `38818148931`:
  `tests/agent_runtime.rs::background_run_returns_process_group_leader_with_child_owned_log`
  panicked at `tests/agent_runtime.rs:1195` - `.expect("runtime PID")` - so the
  `process` identity file was read before it had been written (empty content).
- `#17` main run `38820834406`:
  `tests/ticket_discovery.rs::pi_helper_spawn_survives_an_executable_that_is_open_for_write`
  panicked at `tests/ticket_discovery.rs:263` - the holder thread's
  `OpenOptions::write().open()` failed with `ExecutableFileBusy` (ETXTBSY), so
  the helper was already executing when the holder tried to hold it open.

Reported flaky during the same batch's local runs (3-way parallel load):

- `tests/agent_runtime.rs::cross_process_reset_terminates_a_live_foreground_run`
- `tests/agent_runtime.rs::reserved_background_run_allows_graceful_group_shutdown_before_forcing`
- `tests/agent_runtime.rs::reset_terminates_a_background_pi_follow_up_and_its_descendant`

The ETXTBSY class is known to the code: `src/engine/ticket.rs` already retries a
helper spawn on ETXTBSY (`PI_HELPER_SPAWN_ATTEMPTS`). The *test* that provokes it
races its own writer thread against the spawn, so this race is in the test, not
in the retry.

## Solution

Make the timing/exec assertions deterministic rather than instantaneous:

- Where a test reads a file a helper writes, poll to a bounded deadline (the
  suite already has `wait_for_file`) instead of a single read after a signal.
- In `pi_helper_spawn_survives_an_executable_that_is_open_for_write`, remove the
  holder's own race - establish the hold (file not yet executing) before the
  spawn, or drive the ETXTBSY condition deterministically.
- Consider a serial or retry harness for the process/exec suites specifically
  (`--test-threads=1` for `agent_runtime`, or a bounded retry on the known-flaky
  set), since they share OS-level resources with each other and with the machine.
- **Reproduce first:** run the named tests under load (or raised `--test-threads`)
  until red, then fix against the failing test.

## Acceptance Criteria

- [ ] Each named test is reproduced red under load (or in CI) before it is changed.
- [ ] Each named test passes N consecutive runs under a loaded machine and in CI.
- [ ] No production timing behaviour is changed to make a test pass.
- [ ] `mise run check` is green.

## Out of Scope

- `HELPER_LIVENESS_TIMEOUT` (ticket 35) and the sibling-path isolation (ticket 36)
  - the two earlier flake classes, already fixed.
- Any production change to process-group or session-liveness behaviour.

## References

- `.scratch/bug-hunt/issues/35-load-sensitive-helper-deadlines.md` and
  `36-fixed-sibling-paths-collide.md` (the earlier flake classes)
- `src/engine/ticket.rs` (`PI_HELPER_SPAWN_ATTEMPTS`, the ETXTBSY retry)
- `tests/support/mod.rs` (`wait_for_file`)

## Comments

- 2026-10-16: Filed from the `opinionated-loop` batch, which surfaced it: two of
  the batch's merges left red *main* runs (`#16`, `#17`) on tests unrelated to
  the change. Reopens this effort for one ticket.
