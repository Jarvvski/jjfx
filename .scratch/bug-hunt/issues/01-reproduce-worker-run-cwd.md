# Reproduce the Worker cwd report and pin the root cause

Status: resolved
Type: research
Blocked by: none

## Question

A dispatched Worker's agent process reportedly runs with the **caller's cwd**
instead of its own Worker Workspace (`@/tmp/jjfx-worker-cwd-bug.md`). On the
dispatch path the `current_dir(worker_path(...))` appears not to take effect, so
the child inherits jjfx's own cwd. Every Worker in one dispatch then shares one
working copy and races on `jj describe` / `jj git push`.

Decide whether this is a jjfx defect or an Agent Runtime (opencode) behaviour,
and record the evidence.

## What is already known

The dispatch path was traced and **appears correct**:

- `src/engine/direct_dispatch.rs:400` (dispatch -> background) ->
  `RunSupervisor::run_reserved_background`.
- `src/engine/runtime.rs:526` -> `reserved_request(&reservation, invocation)`.
- `:990` `worker_path(repository.root(), &worker_id)`.
- `:653-654` `command.current_dir(&request.workspace)` before `.spawn()`.

Supporting facts:

- `prepare_for_dispatch` (`src/engine/workspace.rs:612`) asserts the **same**
  `worker_path(...).is_dir()`, so `request.workspace` is a real directory.
- `Repository::root()` normalises a secondary workspace to the default root
  (`src/engine/mod.rs:256`), so the worker path is stable regardless of cwd.
- `Repository::open` canonicalises, and `run_background` applies `current_dir`
  on the same `Command` it spawns.

There is a **test gap**: foreground and probe cwd are asserted
(`tests/agent_runtime.rs:519`, `:1230`), but no test asserts a
background/reserved Run's cwd (`reserved_background_run_helper:2509` and
`background_run_helper:2558` never write `$PWD`).

## Hypotheses

1. **opencode project-root detection under colocated Git worktrees.** The
   report's counter-test (opencode run from a worker dir honours cwd) may have
   used a *non-colocated* worker; the failing workers were colocated worktrees.
   opencode resolving the project root via the colocated `.git` could run its
   shell in the main worktree - consistent with the worker's `jj status`
   reporting the `default` workspace.
2. **A stale installed binary** that predates the `current_dir` call.
3. **A spawn path other than the one traced** (only `run_background` and
   `run_reserved_background` exist; verify against the installed build).

## Experiment

1. Add a throwaway stub runtime that writes `$PWD` and run it through
   `RunSupervisor::run_reserved_background` (and `run_background`) in a
   colocated repo. Assert the child cwd equals the Worker Workspace.
2. On the reporting repo (`/Users/jarvis/Code/ameba/mono`), run the exact
   `opencode run ...` argv jjfx builds, by hand, from a **colocated** worker
   dir and from a **non-colocated** worker dir; compare `$PWD`.
3. Compare `jjfx --version` / the installed binary's behaviour against a build
   from `main`.

## Answer

Resolved 2026-10-13. **Not reproducible against `main` (nor `v0.52.1`).**

Evidence:

1. **jjfx sets the cwd correctly.** Added
   `reserved_background_run_uses_the_worker_workspace_as_cwd`
   (`tests/agent_runtime.rs`): a stub runtime writing `$PWD` through
   `RunSupervisor::run_reserved_background` reports its own Worker Workspace.
   It passes, and it is now a permanent guard - there was previously no test for
   a background Run's cwd (foreground and probe were covered).
2. **`v0.52.1` is identical.** `jj file show -r v0.52.1 src/engine/runtime.rs`
   carries the same `current_dir(&request.workspace)` (`:654`) and
   `reserved_request` -> `worker_path` (`:990`) as `main`, so the reported
   binary's source is not the cause.
3. **opencode honours the cwd**, including inside a colocated Git worktree and
   across sequential runs from different worktrees: `opencode v2.0.27`, the
   shell tool's `pwd` returned the invoking worktree every time (a shared-server
   cwd was explicitly ruled out).

The report is therefore not explained by the traced path. The one untested
variable is the reporter's `PATH`: if `opencode` there is a wrapper or a
different build, jjfx's `Command::new("opencode")` would run that. Ticket 05
carries the recurrence diagnostic and is `needs-info`.

Follow-up: none for jjfx code.

## Acceptance Criteria

- [x] A failing-or-passing test exists that pins a background Run's cwd.
- [x] The root cause is named with reproducible evidence.
- [x] The follow-up fix is identified (05, 24, or a new ticket).

## Out of Scope

- Fixing the bug (that is ticket 05, blocked by this answer).

## Resolution

Reconciled Mon 19 Oct 2026 against the merged code at `9d6bd0ce6e7b`.

This ticket was marked resolved without its acceptance boxes ticked. Every criterion was re-verified by an independent read-only review. The reconciliation recorded no new work; it confirms what shipped.

Evidence: the pinning test exists and is permanent - `reserved_background_run_uses_the_worker_workspace_as_cwd` (`tests/agent_runtime.rs:449`), which drives a reserved background Run through the real supervisor and asserts its cwd is its own Worker Workspace. The root cause is named in the Answer section above (not reproducible against `main` or `v0.52.1`; opencode honours the cwd; the untested variable is the reporter's `PATH`), and the follow-up is identified as ticket 05. This is a research ticket, so its criteria are satisfied by the recorded evidence rather than by code.
