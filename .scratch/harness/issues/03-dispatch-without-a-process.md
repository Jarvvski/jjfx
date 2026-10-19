# Dispatch the initial Run through its session, with no process

Status: ready-for-agent
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

- [ ] An opencode Run starts by prompting its session through the API, with no
      child process spawned, when the API is reachable.
- [ ] The existing process path remains as the fallback when opencode is
      unavailable, unchanged.
- [ ] Liveness, stop and reconciliation behave as they do today (they already
      use the session for opencode).
- [ ] A test drives the no-process path against a fake opencode and asserts no
      process is spawned.

## References

- `src/engine/runtime.rs` (`run_reserved_background`, `prepare_reserved_session`)
- `src/engine/worker_actions.rs` (`deliver_session_prompt`, `FollowUpExecution`)
- `src/engine/opencode_api.rs`
