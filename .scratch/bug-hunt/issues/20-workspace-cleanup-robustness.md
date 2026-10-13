# Harden Workspace and process cleanup

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

Three cleanup paths are unsafe:

- **C4** - a foreground launch failure kills only the direct child
  (`runtime.rs:429-433`, `:443-447`). Foreground Runs are spawned without
  `process_group(0)` (`:411-428`), so agent children (node, git, language
  servers) survive and keep writing to the workspace. Foreground Runs are also
  not their own process group, so a cross-process Reset (`:586-598`) probes a
  non-existent group, gets `ESRCH`, and marks the Worker idle without killing
  it.
- **C5** - `detached_cleanup_markers` (`state.rs:1060-1084`) propagates a
  `WorkerId::parse` error, so a single malformed `.cleanup` filename breaks
  `resize_to` and `destroy`.
- **C6** - `cleanup_untracked_run` (`runtime.rs:1008-1038`) returns `Err` when a
  forced kill cannot confirm exit, but `persist_pid_failure` still releases the
  reservation - leaving the Worker idle and reusable while an untracked agent
  may still be running.

## Fix

- Put foreground Runs in their own process group and kill the **group** on
  failure and Reset.
- Skip (with a diagnostic) unparseable `.cleanup` filenames instead of aborting
  discovery.
- Do not release a reservation while the process group is unconfirmed dead.

## Commits

1. Foreground process group + group kill.
2. Tolerant `detached_cleanup_markers`.
3. Reservation held until the group is confirmed dead.
4. Tests for each path.
5. CHANGELOG entry `(fix)`.

## Acceptance Criteria

- [ ] A failed foreground launch leaves no surviving descendants.
- [ ] A cross-process Reset terminates a live foreground Run.
- [ ] A malformed `.cleanup` file does not break resize/destroy.
- [ ] A reservation is never released while its process group is alive.
- [ ] `mise run check` is green.

## Out of Scope

- Durable Run identity (ticket 07).
