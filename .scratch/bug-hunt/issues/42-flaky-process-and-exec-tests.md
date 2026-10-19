# Process- and exec-timing tests are flaky under CI and load

Status: resolved
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

- [x] Each named test is reproduced red under load (or in CI) before it is changed.
- [x] Each named test passes N consecutive runs under a loaded machine and in CI.
- [x] No production timing behaviour is changed to make a test pass.
- [x] `mise run check` is green.

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
- 2026-10-16: Resolved. Reproduced red under 4-way parallel full-suite load (plus
  24 CPU burners) on a 12-core machine: `background_run_returns_process_group_leader_with_child_owned_log`
  and `cross_process_reset_terminates_a_live_foreground_run` panicked on an empty
  `process`/`descendant` read, and `reserved_background_run_allows_graceful_group_shutdown_before_forcing`
  on an empty `result` read - 11 of 48 full runs failed; separately
  `reset_terminates_a_background_pi_follow_up_and_its_descendant` failed 7 of 40
  runs on an empty PID read. Fix: `tests/support/mod.rs` gains
  `wait_for_file_content`, which polls a helper-written file to the liveness
  deadline instead of reading it the instant the path appears - the file exists
  before it is written, so existence is not enough. The read sites in
  `agent_runtime.rs` and `worker_pool.rs` use it, and the `agent_runtime.rs`
  "process is gone" assertions now poll for the process to be reaped (a
  just-killed descendant is a zombie that `test_kill_process` still reports). No
  production timing changed. Evidence: 64 full-suite runs (4-way parallel) and 80
  concurrent `agent_runtime`+`worker_actions` runs failed 0 (pre-fix: 11/48 and
  7/40), each surviving named test 30/30 green under load, and `mise run check`
  green.
- 2026-10-16: Two of the five named tests were retired mid-flight: PR #24
  (`Remove the Pi Agent Runtime`) deleted `reset_terminates_a_background_pi_follow_up_and_its_descendant`
  (`worker_actions.rs`) and `pi_helper_spawn_survives_an_executable_that_is_open_for_write`
  (`ticket_discovery.rs`, reproduced in CI run `38820834406`) along with the Pi
  helper, so the ETXTBSY write-hold fix that targeted the latter is moot. The
  rebase onto `main` drops both; the three surviving `agent_runtime.rs` tests -
  and the same-class read/reaping races in `worker_pool.rs` - are fixed.

## Re-opened Mon 19 Oct 2026

Reconciled against the merged code at `9d6bd0ce6e7b`. This ticket's own four
criteria held for the five tests it named - `wait_for_file_content`
(`tests/support/mod.rs:85`) polls a helper-written file to the liveness deadline,
and the read sites in `agent_runtime.rs` and `worker_pool.rs` use it - but the
class it exists to remove is not closed, so it is back to `ready-for-agent`. All
boxes are left unticked rather than partially ticked.

The same process/exec flake recurred during the named-pools work in a test this
ticket never covered: `tests/adhoc_ticket.rs::a_markdown_file_outside_scratch_dispatches`
(`tests/adhoc_ticket.rs:140`) failed **once with empty stdout** and passed every
re-run. It spawns the real binary (`CARGO_BIN_EXE_jjfx`, `:72`) and reads its
stdout (`:103`), so it is squarely in this ticket's class - but its failure mode
is not the helper-written-file read race `wait_for_file_content` fixes: the read
is a `wait_with_output()` to EOF (`:103`), which returned nothing at all, so the
lost output points at the process rather than at a file-read window. The
mechanism is not established by inspection and needs reproducing under load.

## Resolution (Mon 19 Oct 2026)

Reproduced red first, and the reproduction named the mechanism. Under the
ticket-35-style load - 16 concurrent `adhoc_ticket` binaries, three continuous
`jjfx` unit-test binaries, and 48 CPU burners on a 12-core machine - 11-12 of
the 16 copies failed in the first round, each at the stdout assertion with empty
stdout. Printing the child's status and stderr from that assertion (kept as a
diagnostic improvement) showed the cause:

    Error: cannot determine which Worker Workspaces are open; re-run with --force to dispatch anyway
    Caused by: running `kitten @ ls` - is kitty's remote control enabled?: kitten @ ls did not finish within 3s

So the empty stdout is neither a lost pipe nor a read race. `wait_with_output()`
reads the child to EOF, and the child printed nothing because Dispatch failed
closed *before* it rendered the batch: the Dispatch Eligibility terminal check
(ADR 0011) spawns the real `kitten @ ls`, and under load the stub terminal was
starved past `terminal::LS_QUERY_TIMEOUT` (3s), so Dispatch refused rather than
risk a Workspace collision.

Ruled out with probes: the pipe/read path (`wait_with_output()` to EOF cannot
lose a written stream, and instrumented timing put every failure at the 3s
deadline, never in the read); the helper-written-file race this ticket's earlier
fix addressed (the Ticket file is written and closed before Dispatch starts, and
the child never reads it back); the child being slow (the stub burns ~5ms of CPU
- timing every `kitten @ ls` under the same load gave successes just under the
deadline, max 2999ms, and timeouts at 3000ms+); and any other subprocess
(`kitten @ ls` is the only process the ad-hoc Dispatch path spawns).

Fix, test-only. `tests/support/mod.rs` gains
`run_jjfx_tolerating_a_starved_terminal` (and a stdin variant), which reruns an
invocation *only* while it failed closed on the terminal refusal, bounded by
`jjfx::test_support::LIVENESS_TIMEOUT` - the shared budget, not a new private
one. Every CLI Dispatch fixture adopts it (`adhoc_ticket`, `dispatch_outcomes`,
`local_tickets`, `pool_routing`, `pool_materialisation`, `pool_spend`), so a
loaded machine can no longer red a Dispatch test that is not about the terminal,
while the seam keeps being exercised; a genuinely stuck or unreachable terminal
still fails within the bound rather than hanging. The named test's assertion now
also prints the child's status and stderr, so a Dispatch that fails before it
prints says why instead of reporting empty stdout. No production timing changed
(`src/` untouched); no changelog entry: nothing user-visible.

Evidence: red first (11-12 of 16 concurrent copies per round pre-fix); green
under the same load (12 consecutive rounds of 16 concurrent `adhoc_ticket`
binaries - 192 binary runs, ~768 Dispatches - plus three continuous unit-test
binaries and 48 burners, 0 failures); `dispatch_outcomes` and the other four
changed fixtures also green under it; `mise run check` green.
`.scratch/bug-hunt/map.md`'s ticket-35 entry is corrected to match that ticket's
`## Resolution`.

One observation outside the named tests: under that extreme harness load,
`tests/repository.rs::concurrent_provisioning_neither_deadlocks_nor_corrupts_the_cache`
exceeded its shared 120s liveness budget once, and passed in isolation and on
the settled re-run. That is the same broad class but a different mechanism - a
real `jj workspace add` taking too long, not the terminal seam - and the load
that redded it (16 concurrent Dispatch binaries plus three continuous unit-test
binaries) is far beyond a real `mise run check`. Recorded, not fixed here.
