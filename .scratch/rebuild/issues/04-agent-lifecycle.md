# Agent lifecycle: hook install, JSONL fold, live agent state

Status: ready-for-agent

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

- [ ] `jjfx hooks install` adds the hook to `~/.claude/settings.json` idempotently and leaves existing hooks intact; a status/check path reports whether hooks are installed.
- [ ] Hooks append one JSON line per event to the global log; concurrent agents do not interleave or corrupt lines.
- [ ] Starting a `claude` session in a workspace moves its row Working -> Waiting live; a permission prompt shows NeedsAttention; ending the session shows Ended/Absent.
- [ ] On startup the TUI reconstructs current agent state by replaying the log; log growth is bounded by size-based rotation.

## Blocked by

- issues/01-spike-hooks.md
- issues/03-skeleton-store.md
