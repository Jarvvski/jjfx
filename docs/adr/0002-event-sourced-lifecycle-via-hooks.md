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

## Spike 01 outcome (validated against Claude Code 2.1.204)

The transitions above are **confirmed**, with refinements
(`.scratch/rebuild/spike-01-hooks-findings.md`):

- `session_id`, `cwd`, `transcript_path` are on every event; `cwd` is the join
  key, as assumed. `Stop` fires once per turn (Waiting), not at session end.
- `PermissionRequest` is a real, first-class event, so `PermissionRequest ->
  NeedsAttention` stands. `Notification` is a broader catch-all keyed by a
  notification type (`permission_prompt`, `idle_prompt`, `agent_needs_input`,
  …); `NeedsAttention` is driven by `PermissionRequest`, with
  `Notification#idle_prompt`/`#agent_needs_input` as optional softer nudges.
- 2.x exposes ~29 hooks (vs. the handful named here); `StopFailure`,
  `PermissionDenied`, and the `Notification` types are available as future
  signals but are not needed for v1.
- Open item: headless `claude -p` cannot surface an interactive permission
  dialog, so the exact stdin fields of `PermissionRequest`/`Notification` are
  documented, not yet captured. Re-capture from a live session before issue 04
  hard-codes their field access.

Status: accepted (validated by spike 01)
