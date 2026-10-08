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
| Pi | `${PI_CODING_AGENT_DIR:-~/.pi/agent}/extensions/jjfx-lifecycle.ts` | jjfx-owned auto-discovered extension |
| OpenCode | `${XDG_CONFIG_HOME:-~/.config}/opencode/plugins/jjfx-lifecycle.ts` | jjfx-owned plugin |

The Claude and Codex hook files are both written unconditionally, so changing
`[agent] command` needs no reinstall; the hook is inert for an agent you never
run.

### Claude Code and Codex

Both register a single append command on the same event names:

- **Claude Code** - `SessionStart`, `UserPromptSubmit`, `Stop`, `SessionEnd`,
  `PermissionRequest`, and `PostToolUse`.
- **Codex** - the same set minus `SessionEnd`. A closed Codex session therefore
  stays `waiting` after its final `Stop` rather than reaching `ended`.

No hook fires when a permission dialog is *resolved*, so `PostToolUse` is also
registered: the first tool completing afterwards is the observable "running
again" signal that clears needs-attention.

### Pi

jjfx installs an owned, auto-discovered extension. Installation is additive and
does not modify Pi settings, packages, sessions, project trust, or unrelated
extensions. A conflicting non-jjfx file at that path is reported and never
overwritten.

The extension maps Pi session start and settled events to waiting, active agent
and turn events to working, and graceful session shutdown to ended. Pi does not
expose a native permission or attention event, so jjfx does not fabricate
`NeedsAttention` from tool or provider failures. An abruptly terminated Pi
process may remain waiting until a later lifecycle event because jjfx does not
infer shutdown by polling or terminal scraping.

### OpenCode

jjfx installs an owned plugin that writes the same versioned lifecycle contract.
It maps `session.created` to a session start, non-idle `session.status` to a
prompt submit (working), `session.idle` to a stop (waiting), `session.deleted`
to a session end, and `permission.asked` to a permission request. Lifecycle
reporting never interrupts an OpenCode session.

## Installation guarantees

- Installs are idempotent and additive. Existing Claude and Codex hooks are kept;
  the jjfx hook is appended alongside them.
- Files jjfx owns (the Pi extension and OpenCode plugin) carry a marker and are
  updated in place when they are outdated.
- A conflicting non-jjfx file at an owned path is reported by
  `jjfx hooks status` and never overwritten.
