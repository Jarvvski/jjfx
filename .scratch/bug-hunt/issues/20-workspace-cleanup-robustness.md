# Harden Workspace and process cleanup

Status: ready-for-agent
Type: task
Blocked by: none

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
