# Fix the Worker Run working directory

Status: needs-info
Type: task
Blocked by: 01

## Problem

The Worker-cwd report (`@/tmp/jjfx-worker-cwd-bug.md`) is **not reproducible**
against `main` or `v0.52.1` (ticket 01). jjfx sets the Run cwd correctly, and
opencode honours it, including in a colocated Git worktree.

Ticket 01 landed `reserved_background_run_uses_the_worker_workspace_as_cwd` as a
permanent guard, so a regression in jjfx's spawn path is now caught. There is no
jjfx fix to make without a reproduction.

## Recurrence diagnostic

If a Worker still runs in the caller's cwd, capture:

1. `which -a opencode` and `file "$(command -v opencode)"` - is it a wrapper or
   a different build?
2. `jjfx --version` and the commit the running binary was built from.
3. `ps -o pid,ppid,pgid,command=` for the whole worker process tree.
4. `lsof -a -p <pid> -d cwd` for the agent **and** each of its children.
5. `jjfx pool list` and the worker's `.jj/pool/<worker>.log`.

## Acceptance Criteria

- [ ] The reporter's `which -a opencode` and binary provenance are known.
- [ ] If a wrapper or a non-jjfx cause is confirmed, this ticket closes as
      environment with the evidence recorded.
- [ ] If jjfx is at fault, this becomes a fix with a failing test.

## Out of Scope

- The secondary-workspace root bug (I5, ticket 11).
