# `jjfx logs` follows an empty file for a session-delivered Run

Status: resolved
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

- [x] `jjfx logs <worker>` shows a session-delivered Run's activity (e.g. by
      streaming the session's events), or `status`/`logs` say plainly that no log
      exists for a session-delivered Run and where to look instead.
- [x] The skill's "watch" step matches what the command actually does.

## References

- `.scratch/harness/issues/03-dispatch-without-a-process.md`
- `src/engine/run_log.rs`, `src/engine/opencode_api.rs`

## Triage (2026-10-20)

Triaged from `needs-triage` to `ready-for-agent`. The evidence is a **live
reproduction during the second drive**, not a code reading: the drive hit this
while working a real backlog through jjfx, and the report
(`.scratch/dogfood-2/report.md`) records what happened around it.

Not independently re-verified - the drive reproduced it live.

The acceptance criteria are unchanged - they were already specific and
testable, which is why this needed triage rather than a rewrite.

## Resolution

Streaming, not the fallback: `jjfx logs` now follows a session-delivered Run's
opencode session and prints its activity.

`WorkerActions::logs` decides the source. A Run normally writes a structured
process log; a Run delivered through its opencode session writes none, so the
log it would have written stays empty. When a Worker's runtime is opencode, its
log is empty, and the Run's session can be identified (the id `harness/03`
already records, or a unique active title match), `WorkerLogs::session` names
the session instead of the file. An opencode Run whose *process* wrote a log
keeps following that log - the log has to be empty for the session to win - so
nothing that works today changed behaviour.

Reading the session is `OpenCodeApi::session_messages`, which asks
`session.message.list` for the newest page (`order=desc`, bounded `limit`) and
returns it oldest-first as typed `SessionMessage`s. Their activity is parsed by
`run_log::opencode_message_activities`, beside the other provider adapters, into
the same provider-neutral `RunActivity` the process log produces - so both
sources render through one `render_activity`. Assistant text, reasoning and tool
parts (streaming/running/completed/error) and session `shell` messages map onto
it; prompts, system notes and the idle marker report nothing.

`logs` prints the newest activity as a tail and then follows, printing each new
or changed line exactly once. A message is not immutable - an assistant message
grows as its content streams, and a tool part moves from running to completed in
place - so the follower tracks the rendered lines per message (`SessionFeed`)
rather than which messages it has seen, and stops when the Run reaches a
terminal state on the same `follow_stop` check the log path uses.

When the daemon cannot be read, the command says so plainly and names where to
look instead, rather than following an empty file forever:

    No process log for worker-1be12c: its Run is delivered through opencode session
    ses_... , whose activity could not be read (opencode API unavailable: ...).
    Follow that session from opencode, or re-run once its server is reachable.

The skill's watch step now says which of the two the banner names, and that
`log_file` is empty and not what `logs` follows for a session-delivered Run;
`PLAYBOOK.md`'s finding-11 entry is marked done.

Verified: `mise run check` (changelog validation + fmt + lint + build + test)
passes. Unit tests cover the message parser and the follower's dedupe; the
integration tests in `tests/logs.rs` drive a fake `opencode` and cover a
session-delivered Run's activity, following one to its end, a session that
cannot be read, and an opencode Run whose process log is still followed.

