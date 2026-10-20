# Agent lifecycle integration

jjfx shows each workspace's live agent state (working, waiting, needs-attention,
ended) by observing agent lifecycle events. Two commands manage the integration:

```bash
jjfx hooks status    # report, per agent, what is installed and what is missing
jjfx hooks install   # install or update every integration
```

Both are global - they manage files under your home directory and need no jj
repository. Events are appended to one shared log at
`${XDG_STATE_HOME:-~/.local/state}/jjfx/events.jsonl`, which jjfx tails for live
transitions and bounds with size-based rotation.

## What gets installed

| Agent | File | Mechanism |
| --- | --- | --- |
| Claude Code | `~/.claude/settings.json` | Append-only JSON hooks |
| Codex | `~/.codex/hooks.json` | Append-only JSON hooks |
| OpenCode | `${XDG_CONFIG_HOME:-~/.config}/opencode/plugins/jjfx-lifecycle.ts` | jjfx-owned plugin |

The Claude and Codex hook files are both written unconditionally, so changing
`[agent] command` needs no reinstall; the hook is inert for an agent you never
run.

### Claude Code and Codex

Both register a single append command on the same event names:

- **Claude Code** - `SessionStart`, `UserPromptSubmit`, `Stop`, `SessionEnd`,
  `PermissionRequest`, `PostToolUse`, `Notification`, `StopFailure`,
  `SubagentStart`, `SubagentStop`, `PostToolBatch`, `TaskCreated`,
  `TaskCompleted`, and `TeammateIdle`.
- **Codex** - `SessionStart`, `UserPromptSubmit`, `Stop`, `PermissionRequest`,
  `PostToolUse`, `Interrupt`, `SubagentStart`, and `SubagentStop`. A closed
  Codex session therefore stays `waiting` after its final `Stop` rather than
  reaching `ended`.

No hook fires when a permission dialog is *resolved*, so `PostToolUse` is also
registered: the first tool completing afterwards is the observable "running
again" signal that clears needs-attention.

Two of the registered events are catch-alls discriminated by a payload field
rather than the event name, so the config registers them once and the Rust fold
reads the type:

- **`Notification`** carries `notification_type`. `agent_needs_input` is a
  genuine blocked-on-a-human signal mapped to needs-attention; the other types
  (`permission_prompt`, `agent_completed`, `idle_prompt`, ...) are covered by
  `PermissionRequest` and `Stop` and leave the state alone.
- **`StopFailure`** carries `error`. `rate_limit` and `billing_error` are a
  distinct **quota-blocked** outcome (blocked on provider capacity or billing,
  not on a bug), shown in the workspace list and never auto-retried by
  orchestration; any other error keeps the ordinary finished-turn mapping.

`SubagentStart`/`SubagentStop`, `PostToolBatch`, `TaskCreated`/`TaskCompleted`,
and `TeammateIdle` are recorded for live progress on long Runs; they never move
the agent state.

### OpenCode

jjfx installs an owned plugin that writes the same versioned lifecycle contract.
It maps `session.created` to a session start, non-idle `session.status` to a
prompt submit (working), `session.idle` to a stop (waiting), `session.deleted`
to a session end, and `permission.asked` to a permission request. A v2 opencode
reports a turn with `session.execution.started` and
`session.execution.succeeded`/`failed`/`interrupted` instead of the
status/idle pair; the plugin maps those too, to the same states. Lifecycle
reporting never interrupts an OpenCode session.

The plugin also reports **parentage**. opencode nests sessions: a child session
is one created with a `parentID`, and jjfx creates a Worker's session as a child
of the session that dispatched it (the `OPENCODE_SESSION_ID` opencode puts in
that session's shell environment) **when that session is already anchored to the
Worker Workspace**. opencode anchors a child session to its parent's directory
and offers no way to move it, so a Dispatch from a session in the repository -
an agent's own shell - creates a root session anchored to the Worker Workspace
instead; the anchor is the guard that keeps a Run out of the wrong worktree. A
Dispatch from outside opencode - the TUI, a terminal, CI - creates a root session
too. The plugin remembers each session's parent when it is created and stamps
`parent_id` on that session's whole lifecycle, so a parent can enumerate and read
its children's state rather than polling.

## Installation guarantees

- Installs are idempotent and additive. Existing Claude and Codex hooks are kept;
  the jjfx hook is appended alongside them.
- Files jjfx owns (the OpenCode plugin) carry a marker and are
  updated in place when they are outdated.
- A conflicting non-jjfx file at an owned path is reported by
  `jjfx hooks status` and never overwritten.
