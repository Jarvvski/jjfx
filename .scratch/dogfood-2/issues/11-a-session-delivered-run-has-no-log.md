# `jjfx logs` follows an empty file for a session-delivered Run

Status: needs-triage
Type: bug
Blocked by: none

## Problem

`harness/03` made an opencode Run start by prompting its session through the API,
with **no client process**. The Worker's log file was written by the process, so
for a session-delivered Run it stays empty, and `jjfx logs` follows nothing:

    $ ls -l .jj/pools/default/worker-1be12c.log
    -rw-r--r-- 1 jarvis staff 0 20 Oct 14:53 .jj/pools/default/worker-1be12c.log

    $ jjfx logs worker-1be12c
    Following opencode log for worker-1be12c
    # ... no further output, ever, for a Run that ran 4m30s ...

The skill still presents `jjfx logs worker-01` as how you "follow one Worker's
activity" (`skills/jjfx-dispatch/SKILL.md`, step 4), and `status --json` still
reports a `log_file` for the Worker. Both now point at a file that will never
have content for the common (opencode, API-reachable) case.

## Acceptance Criteria

- [ ] `jjfx logs <worker>` shows a session-delivered Run's activity (e.g. by
      streaming the session's events), or `status`/`logs` say plainly that no log
      exists for a session-delivered Run and where to look instead.
- [ ] The skill's "watch" step matches what the command actually does.

## References

- `.scratch/harness/issues/03-dispatch-without-a-process.md`
- `src/engine/run_log.rs`, `src/engine/opencode_api.rs`
