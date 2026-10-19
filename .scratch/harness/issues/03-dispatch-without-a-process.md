# Dispatch the initial Run through its session, with no process

Status: resolved
Type: task
Blocked by: none

## Problem

jjfx already drives opencode through its API: it creates a session before
launch, records its id durably, sets `--session <id>` on the spawned process,
stops a Run *through its session*, and decides an opencode Run is finished by
its session outcome rather than a pid probe.

The **follow-up** path already goes further - `FollowUpExecution::Session`
delivers a prompt with `prompt_async` and **drives no process at all**. Only the
*initial* dispatch still spawns `opencode run` as a detached process group.

That process is the last thing keeping a Worker a process rather than a session,
and it is the reason a Worker needs a pid, a process group and a reaper thread.
The seam to remove it already exists and is proven.

## Acceptance Criteria

- [x] An opencode Run starts by prompting its session through the API, with no
      child process spawned, when the API is reachable.
- [x] The existing process path remains as the fallback when opencode is
      unavailable, unchanged.
- [x] Liveness, stop and reconciliation behave as they do today (they already
      use the session for opencode).
- [x] A test drives the no-process path against a fake opencode and asserts no
      process is spawned.

## References

- `src/engine/runtime.rs` (`run_reserved_background`, `prepare_reserved_session`)
- `src/engine/worker_actions.rs` (`deliver_session_prompt`, `FollowUpExecution`)
- `src/engine/opencode_api.rs`

## Resolution

`RunSupervisor::run_reserved_background` now returns a `BackgroundStart`: an
opencode Run whose session the daemon accepts the prompt for (through
`OpenCodeApi::prompt_async`) starts as `BackgroundStart::Session` and spawns no
client process; every other Run - and an opencode Run whose API is unreachable,
older, or refuses - starts as `BackgroundStart::Process`, the path that predates
this, unchanged. The decision is made after `prepare_reserved_session`, which
already creates and records the Run's session, so this is an extension of the
existing opencode special case rather than a rewrite: reconcile and stop already
consult the session, and a session-delivered Run simply leaves the Worker `busy`
with no pid for that existing path to finish.

The frontends learned the new shape: `DirectDispatchExecution::Session` and
`DispatchOutcome::session` carry the session instead of a pid, and the CLI, TUI,
adapter and orchestration event render it. The follow-up path's background
fallback goes through the same method, so a fresh opencode follow-up (no session
to resume) now also creates a session and prompts it with no client process.

Verified: `mise run check` (changelog validation + fmt + lint + build + test)
passes. `opencode_run_prompts_its_session_with_no_client_process` drives a fake
`opencode` on `PATH` and asserts no `opencode run` process was spawned;
`opencode_run_falls_back_when_the_session_api_is_unavailable` covers the
fallback.
