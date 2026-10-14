# Fix the Worker Run working directory

Status: resolved
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

- [x] The reporter's `which -a opencode` and binary provenance are known.
- [x] If a wrapper or a non-jjfx cause is confirmed, this ticket closes as
      environment with the evidence recorded.
- [~] If jjfx is at fault, this becomes a fix with a failing test. *Not triggered -
      jjfx is not at fault.*

## Outcome (2026-10-14)

**Closed as an upstream opencode defect, not a jjfx one.** Finding 1 does not
reproduce, and the cause is now identified.

- **opencode is not a wrapper.** `which -a opencode` →
  `/opt/homebrew/bin/opencode`, a Mach-O arm64 binary symlinked to
  `../Cellar/opencode-v2/2.0.27/bin/opencode`. The ticket's leading hypothesis is
  refuted.
- **jjfx sets the Run cwd correctly, including in the released binary.** `v0.52.1`'s
  own tree contains `.current_dir(&request.workspace)` at `src/engine/runtime.rs:419`
  and `:654` - the same lines as `main`. The report's "the `current_dir(...)` appears
  not to take effect" was a misdiagnosis.
- **The bug does not reproduce on opencode 2.0.27.** A session started from
  `mono-workspaces/worker-8f9bdd` runs its shell tools in that workspace, with both
  the shared daemon and `--standalone`; a fresh colocated repo behaves the same. The
  session directory now honours the process cwd.
- **The 13 Oct incident matches opencode v2's session-directory mishandling**, not
  git resolution. Sessions were being set to a literal `global` (the owner's
  `~/.local/share/opencode/revert-global-sessions-*.txt` repair lists) or to the
  background daemon's cwd (`/Users/jarvis`, which accounts for 66 of the entries
  repaired). That is what put the worker's tools in `mono`. The report's own control
  test (`Run pwd and return exact output`, 13 Oct 11:14) was caught by the same bug,
  which is why it appeared to clear opencode.
- `opencode session list` still *groups* sessions by git project, so old worker
  sessions show under `mono`; that grouping is cosmetic and separate from the
  directory tools actually run in.

The recurrence diagnostic above stays as written and remains the right thing to
capture if this ever returns.

**Carried forward.** The report's *Finding 2* (stopping a Worker does not stop its
work) is real and is a jjfx concern: without `--standalone`, the recorded process is
a daemon client, not the work. It is recorded in ticket 07 (see its 2026-10-14 note)
and belongs with 07/20's Run-identity and process-group work.

## Out of Scope

- The secondary-workspace root bug (I5, ticket 11).
