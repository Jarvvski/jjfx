# Harden Workspace and process cleanup

Status: resolved
Type: task
Blocked by: none

## Reclassified (2026-10-13)

Moved from `ready-for-agent` to `ready-for-human`. C5 landed, and both remaining
items are blocked on a decision rather than on work:

- **C4** needs the owner to choose between three options (below). The correct one
  requires `tcsetpgrp` around the foreground Run, which is a change to the CLI's
  terminal handling, not a spawn flag.
- **C6** needs a test strategy. Its failure mode requires a process group to
  survive `SIGKILL`, which cannot be arranged deterministically, so the fix would
  land unverified - which this effort does not do.

Everything actionable in this ticket is done.

## Progress (2026-10-13)

**C5 is fixed.** `detached_cleanup_markers` no longer propagates a
`WorkerId::parse` error: a marker whose stem is not a valid Worker id cannot name
a Pool member, so it is skipped. One junk file used to fail discovery, and with
it every `resize`/`destroy`.

Committed as `Ignore cleanup markers that cannot name a Worker`, with
`destroy_ignores_a_cleanup_marker_that_cannot_name_a_worker` (verified failing on
the pre-fix code).

**C4 and C6 remain, and both need a decision or a test strategy** (below).

## Problem

Three cleanup paths were unsafe:

- **C4** - a foreground launch failure kills only the direct child
  (`runtime.rs:429-433`, `:443-447`). Foreground Runs are spawned without
  `process_group(0)` (`:411-428`), so agent children (node, git, language
  servers) survive and keep writing to the workspace. Foreground Runs are also
  not their own process group, so a cross-process Reset (`:586-598`) probes a
  non-existent group, gets `ESRCH`, and marks the Worker idle without killing
  it.
- **C5** - `detached_cleanup_markers` (`state.rs`) propagated a
  `WorkerId::parse` error, so a single malformed `.cleanup` filename broke
  `resize_to` and `destroy`. **Fixed.**
- **C6** - `cleanup_untracked_run` (`runtime.rs:1008-1038`) returns `Err` when a
  forced kill cannot confirm exit, but `persist_pid_failure` still releases the
  reservation - leaving the Worker idle and reusable while an untracked agent
  may still be running.

## C4: Decision Needed

The obvious fix - spawn foreground Runs with `process_group(0)` and kill the
group - **breaks interactive foreground Runs**. A foreground Run inherits stdin
(`runtime.rs:420`) so the agent can read the terminal. Moving it into its own
process group takes it out of the terminal's foreground group, so a read raises
`SIGTTIN` and the agent stops. Doing this correctly also requires `tcsetpgrp`
(hand the terminal to the child, take it back on exit), which is a change to the
CLI's terminal handling, not a one-line spawn flag.

Options:

1. Add `process_group(0)` **and** `tcsetpgrp` around the foreground Run.
2. Add `process_group(0)` only, accepting that a foreground agent that reads
   stdin is stopped.
3. Accept the current behaviour and document that a foreground Run's descendants
   are not group-killed.

## C6: Test Strategy Needed

The fix is small (release only when cleanup succeeded), but its failure mode is
**not deterministically testable**: `terminate_process_group` escalates to
`SIGKILL`, so a process group cannot be made to survive it. The only realistic
trigger is a recycled process-group id, which is exactly the untestable case.

The change is a four-line ordering guarantee, so it is left for a pass that
decides how to verify it (e.g. by injecting the cleanup outcome), rather than
landing unverified.

## Acceptance Criteria

- [x] A malformed `.cleanup` file does not break resize/destroy.
- [x] A failed foreground launch leaves no surviving descendants.
- [x] A cross-process Reset terminates a live foreground Run.
- [x] A reservation is never released while its process group is alive.
- [x] `mise run check` is green.

## Out of Scope

- Durable Run identity (ticket 07).

## Comments

**Decision (2026-10-14): C4 option 1, and C6 verified by injecting the cleanup
outcome.**

- **C4 - own process group plus `tcsetpgrp`.** Foreground Runs are spawned with
  `process_group(0)` and the terminal is handed to the child with `tcsetpgrp`,
  taken back on exit. Verified feasible without `unsafe`: `rustix` 1.x exposes
  safe `tcsetpgrp`/`tcgetpgrp` in `rustix::termios`, and its `termios` feature is
  a bare `termios = []` with no dependencies - so this is one word added to an
  existing dependency's feature list, not a new crate. This also makes group-based
  liveness sound for *every* Run, which is what ticket 07 needs.
- **C6 - inject the cleanup outcome.** The ordering guarantee is not
  deterministically testable (a process group cannot be made to survive
  `SIGKILL`), so the cleanup outcome becomes injectable and the four-line ordering
  is pinned through that seam rather than landing unverified.

## Outcome (2026-10-14)

All three items landed.

**C5** was fixed earlier (`Ignore cleanup markers that cannot name a Worker`).

**C4** - `Give every Run its own process group and probe Runs by group leadership` plus `Hand the terminal to a foreground Run and back from its own group`. Every Run is spawned with `process_group(0)`, and the two foreground failure paths end the whole group through `terminate_foreground_run` instead of `child.kill()`.

The decision's option 1 needed more than `tcsetpgrp` from the supervisor: taking the terminal *back* from a background group raises `SIGTTOU`, and the only ways to suppress it (`SIG_IGN`, or blocking the signal) need `unsafe` or a new dependency - verified by experiment on macOS, and `rustix`'s signal APIs are Linux-only. So the terminal is handed back by the Run's own group instead: the supervisor spawns a hidden `jjfx __foreground` wrapper with `process_group(0)`, makes its group foreground, and the wrapper runs the Agent in that group, waits, then restores the captured group before exiting. Because the wrapper is the foreground group while it hands back, `SIGTTOU` is never raised, with no `unsafe` and no new crate.

Evidence: `cross_process_reset_terminates_a_live_foreground_run` (red pre-fix - `terminated_pid: None`, the group was never signalled), `failed_foreground_launch_leaves_no_surviving_descendants` (red pre-fix - a surviving descendant), and `foreground_run_reads_the_terminal_and_returns_it`, a real-PTY test where the Agent reads the terminal (which stopped on `SIGTTIN` pre-fix) and the supervisor reads it again after the Run, asserting the hand-back and the exit code.

**C6** - `Release a Reservation only after its untracked Run is gone`. `persist_pid_failure` is split into `persist_pid_failure_with(reservation, background, primary, cleanup)`; the Reservation is released only on `Ok`, and an unconfirmed cleanup returns `PersistPidCleanup` and keeps it. Evidence: `a_reservation_survives_an_unconfirmed_untracked_cleanup` (red pre-fix - `Idle` where the domain expects `Busy`), driving a real Repository, Worker Reservation and background process, injecting only the cleanup outcome through the seam.

Costs and residual risk: the handover adds one wrapper process per tty foreground Run (transparent for the log and the exit code) and needs `rustix`'s `pty` and `stdio` features for the PTY test, which - because cargo unifies features - also build into production. A tty foreground Run now records the wrapper's pid (it leads the group) rather than the Agent's; it is still a live group leader, so Reset and `reconcile_worker` are unchanged. Ctrl-C reaches the Agent only, since the Run's group is the foreground group. The `wait_until_foreground` 10s deadline is a safety net and is untested; a failing PTY test leaves its helper processes behind. Verified on macOS only - `tcsetpgrp`/`SIGTTOU` are POSIX, but nothing was run on Linux.
