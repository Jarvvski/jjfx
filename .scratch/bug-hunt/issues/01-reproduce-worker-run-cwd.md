# Reproduce the Worker cwd report and pin the root cause

Status: ready-for-agent
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

(Fill in when resolved.) State the root cause, the evidence, and which of
tickets 05 / 24 / a new opencode-integration ticket follows.

## Acceptance Criteria

- [ ] A failing-or-passing test exists that pins a background Run's cwd.
- [ ] The root cause is named with reproducible evidence.
- [ ] The follow-up fix is identified (05, 24, or a new ticket).

## Out of Scope

- Fixing the bug (that is ticket 05, blocked by this answer).
