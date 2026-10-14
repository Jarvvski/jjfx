# Harden Workspace and process cleanup

Status: ready-for-agent
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
- [ ] A failed foreground launch leaves no surviving descendants.
- [ ] A cross-process Reset terminates a live foreground Run.
- [ ] A reservation is never released while its process group is alive.
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
