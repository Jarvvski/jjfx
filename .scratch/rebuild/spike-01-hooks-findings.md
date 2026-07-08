# Spike 01 findings: Claude Code hook lifecycle events

Type: findings
Status: done
Parent: issues/01-spike-hooks.md
Claude Code version under test: **2.1.204**

## Method

Empirical capture with a throwaway hook rig (no changes to `~/.claude`):

- A `capture.sh` appends each hook's verbatim stdin to a log, tagged by event.
- A `--settings` JSON registers that script on every candidate event.
- Nested headless runs: `claude -p "<prompt>" --settings <file> --session-id
  <uuid> --permission-mode <mode> --output-format stream-json
  --include-hook-events --verbose`, each in its own throwaway `cwd`.
- Four runs: (1) no-tool prompt, (2) auto-approved Bash, (3) default-mode Bash,
  (4) Bash denied via `permissions.deny` to try to force a permission gate.

Payload shapes for the interactive-only events (PermissionRequest, Notification)
are taken from the authoritative reference at <https://code.claude.com/docs/en/hooks>
- see the limitation below.

## What fired, captured verbatim

All six deterministic lifecycle events fired and every payload carried
`session_id`, `cwd`, `transcript_path`, and `hook_event_name`:

```jsonc
// SessionStart
{"session_id":"…","transcript_path":"…","cwd":"…","hook_event_name":"SessionStart","source":"startup"}
// UserPromptSubmit
{"session_id":"…","transcript_path":"…","cwd":"…","prompt_id":"…","permission_mode":"auto","hook_event_name":"UserPromptSubmit","prompt":"…"}
// PreToolUse
{…,"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"echo …","description":"…"},"tool_use_id":"toolu_…"}
// PostToolUse
{…,"hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{…},"tool_response":{"stdout":"…","stderr":"","interrupted":false,"isImage":false,"noOutputExpected":false},"tool_use_id":"toolu_…","duration_ms":29}
// Stop
{…,"hook_event_name":"Stop","stop_hook_active":false,"last_assistant_message":"hello","background_tasks":[],"session_crons":[]}
// SessionEnd
{…,"hook_event_name":"SessionEnd","reason":"other"}
```

Notes:

- **`session_id` + `cwd` are on every event** (confirmed empirically on the six
  above; the reference lists both as common input fields on all hooks). `cwd`
  is the clean join key to a workspace - no title matching needed (ADR 0002).
- **`Stop` fires once per turn**, not at session end - it is the "turn finished,
  awaiting the human" signal. It carries `last_assistant_message`.
- `SessionStart.source` is one of `startup|resume|clear|compact`; `SessionEnd.reason`
  observed `other` (also `clear|logout|prompt_input_exit|…` per docs).
- `UserPromptSubmit`/`Stop` also carry `permission_mode` and (sometimes) `effort`.

## Notification vs PermissionRequest - resolved

Both are **real, first-class hook events** in 2.x (the reference enumerates ~29
events). The ambiguity resolves as:

- **`PermissionRequest`** - "When a permission dialog appears." This is the
  direct, authoritative **blocked-on-a-human-decision** signal -> `NeedsAttention`.
  ADR 0002's `PermissionRequest -> NeedsAttention` mapping is therefore **valid**,
  not a phantom.
- **`Notification`** - a catch-all, keyed by a notification type used as the
  hook matcher: `permission_prompt`, `idle_prompt`, `auth_success`,
  `elicitation_dialog`, `elicitation_complete`, `elicitation_response`,
  `agent_needs_input`, `agent_completed`. `Notification#permission_prompt`
  coincides with `PermissionRequest`; `#idle_prompt` / `#agent_needs_input` are
  distinct "needs-you" nudges with no permission dialog.

Decision for ticket 04: drive `NeedsAttention` from **`PermissionRequest`** (the
direct event); optionally also treat `Notification#idle_prompt` /
`#agent_needs_input` as softer needs-you nudges. Do not depend on parsing message
text - the notification type is the programmatic discriminator.

## Event -> agent-state transition map (for issue 04)

| Hook | Agent transition | Notes |
| --- | --- | --- |
| `SessionStart` | session opens (presence begins) | `source` distinguishes startup/resume/clear/compact |
| `UserPromptSubmit` | -> **Working** | turn begins; carries `prompt` |
| `Stop` | -> **Waiting** | once per turn; carries `last_assistant_message` |
| `StopFailure` | -> **Waiting** | turn ended on an API error (secondary) |
| `PermissionRequest` | -> **NeedsAttention** | blocked on a permission dialog |
| `Notification` (`idle_prompt`/`agent_needs_input`) | -> **NeedsAttention** (soft nudge) | no dialog; optional |
| `SessionEnd` | -> **Ended** | `reason` distinguishes logout/exit/clear/… |

Working is the interval between `UserPromptSubmit` and the next `Stop` - there is
no continuous "generating" signal, exactly as ADR 0002 assumed.

## Limitation (must close before shipping ticket 04's NeedsAttention path)

Headless `claude -p` **cannot** reproduce an interactive permission dialog or an
idle notification: run 3 (default mode) auto-approved the tool, and run 4 (Bash
denied) made the model route around Bash rather than surface a gate - in both,
`permission_denials` was empty and neither `PermissionRequest` nor `Notification`
fired. Their **exact stdin field shapes are therefore documented, not captured.**
Before ticket 04 hard-codes `PermissionRequest`/`Notification` field access,
re-capture one real payload from a live interactive session (the rig here works
unchanged - just install the hooks and trigger a real permission prompt).

## Impact on ADR 0002

Reality **confirms** ADR 0002's core transitions and its `cwd`-join premise; the
ADR is amended only to record the spike outcome, the richer 2.x event set, and
this one open item. No downstream ticket needs to change.
