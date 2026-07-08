# Agent lifecycle: hook install, JSONL fold, live agent state

Status: done

## Parent

epics/B-triage-and-actions.md

## What to build

The event-sourced spine (ADR 0002/0004). A `jjfx hooks install` command writes
the dumb append-only hook into `~/.claude/settings.json` (idempotent,
non-destructive to existing hooks). At runtime the TUI tails the global JSONL
event log, folds events into a per-workspace agent lifecycle (Absent/Working/
Waiting/NeedsAttention/Ended) keyed by `cwd`, rebuilds current state by replaying
the log on startup, and shows the agent state on each workspace row. Uses the
event -> transition map confirmed in issue 01.

## Acceptance criteria

- [x] `jjfx hooks install` adds the hook to `~/.claude/settings.json` idempotently and leaves existing hooks intact; a status/check path reports whether hooks are installed. (Verified: `install` twice reports "already installed"; `merge_preserves_existing_hooks_and_keys` test keeps unrelated keys + pre-existing Stop hook; `hooks status` lists installed/missing.)
- [x] Hooks append one JSON line per event to the global log; concurrent agents do not interleave or corrupt lines. (Verified: 50 concurrent `sh -c` hook writes -> 50 valid JSON lines, zero corruption; single `printf '%s\n' "$(cat)"` = one `O_APPEND` write below `PIPE_BUF`.)
- [x] Starting a `claude` session in a workspace moves its row Working -> Waiting live; a permission prompt shows NeedsAttention; ending the session shows Ended/Absent. (Verified via PTY: appending `UserPromptSubmit` flips the row to "working" live, `Stop` to "waiting"; `PermissionRequest`->NeedsAttention and `SessionEnd`->Ended covered by `transitions_follow_the_spike_map`. Only common fields `hook_event_name`/`cwd` are read, sidestepping spike 01's un-captured field shapes.)
- [x] On startup the TUI reconstructs current agent state by replaying the log; log growth is bounded by size-based rotation. (Verified: `read_events` + `fold` rebuild state on launch; `rotate_if_needed` keeps the tail from a line boundary under the 4 MiB cap - `rotate_keeps_the_tail_from_a_line_boundary` test.)

## Blocked by

- issues/01-spike-hooks.md
- issues/03-skeleton-store.md
