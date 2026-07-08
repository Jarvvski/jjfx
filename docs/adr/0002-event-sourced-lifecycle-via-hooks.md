# Drive the agent lifecycle from Claude Code hooks, not the process tree

Agent-session state is event-sourced from Claude Code hooks rather than inferred
by polling the terminal and process tree (the original tool guessed "working"
from a `caffeinate` process under kitty). Hooks fire at exact lifecycle
boundaries and every payload carries `session_id`, `cwd`, and `transcript_path`,
so `cwd` cleanly joins an event to a workspace with no title matching.

Transitions: `UserPromptSubmit` -> Working, `Stop` -> Waiting,
`PermissionRequest` -> NeedsAttention, `SessionStart` / `SessionEnd` -> session
boundaries. There is no continuous "generating" signal, so Working is the
interval between a prompt and its stop.

## Consequences

- Requires installing global hooks in `~/.claude/settings.json` and an event
  transport from the hook process to the running TUI.
- Decouples state detection from kitty and from macOS `caffeinate` entirely.
- The TUI is useless for lifecycle state until the hooks are installed; setup
  must install/verify them (or degrade gracefully to a coarse fallback).

Status: accepted
