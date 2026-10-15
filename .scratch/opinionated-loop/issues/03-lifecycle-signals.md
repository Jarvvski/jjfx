# Ingest the agent lifecycle signals jjfx ignores

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

jjfx installs lifecycle hooks into every runtime (`docs/lifecycle-integration.md`,
`src/hooks.rs`) but registers only a subset, and the ignored events carry exactly
the distinctions the Attention collapse (`CONTEXT.md`) cannot express today.

- **Claude Code** exposes `Notification` (matchers include `agent_needs_input`,
  `agent_completed`, `idle_prompt`), `StopFailure` (matchers `rate_limit`,
  `overloaded`, `billing_error`, `max_output_tokens`), `SubagentStart`/`SubagentStop`,
  `PostToolBatch`, `TaskCreated`/`TaskCompleted`, `TeammateIdle`. jjfx registers
  `SessionStart`, `UserPromptSubmit`, `Stop`, `SessionEnd`, `PermissionRequest`,
  `PostToolUse`.
- **Codex** exposes `Interrupt`, `SubagentStart`/`SubagentStop`, and equivalents.

The Agent lifecycle is Absent / Working / Waiting / NeedsAttention / Ended. It
cannot say **quota-blocked**, and it cannot distinguish *blocked on a human* from a
plain finished turn. So a rate-limited Run reads as `failed` (and orchestration
retries hammer the quota wall), and a needs-input Worker does not sort to the top
with the right reason.

## Solution

Extend the installed hook contract and the shared `events.jsonl` schema
(`src/events.rs`, ADR 0002/0004) to carry the richer signals, then map them into
the lifecycle. This is an extension of an existing contract, not new plumbing.

- Map `Notification:agent_needs_input` to `NeedsAttention`.
- Add a **quota-blocked** outcome (from `StopFailure:rate_limit` /
  `billing_error`) distinct from `failed`, and make orchestration treat it as a
  wait, not a retry-now.
- Carry `SubagentStart`/`Stop` and `PostToolBatch` for live progress on long Runs.

## Commits

1. Extend the hook installer and event schema for the new Claude/Codex events,
   keeping the existing events and their consumers working.
2. Map the new events into the Agent lifecycle, adding the quota-blocked outcome.
3. Surface quota-blocked and needs-input in the workspace list and the Attention
   collapse, and stop the orchestration retry loop from retrying a quota-blocked
   Run.
4. Tests over a fake hook emitter; changelog.

## Acceptance Criteria

- [ ] `Notification:agent_needs_input` produces `NeedsAttention`.
- [ ] A rate-limit/billing `StopFailure` produces a distinct quota-blocked
      outcome, shown in the list, and is not retried immediately.
- [ ] Subagent/`PostToolBatch` events are recorded and do not regress existing
      Working/Waiting transitions.
- [ ] The event log stays backward compatible for existing consumers.
- [ ] `mise run check` is green.

## Out of Scope

- opencode's SSE event bus (ticket 04).
- Notification/desktop alerts (a separate idea; not filed here).
- Any change to the Work lifecycle.

## References

- Claude Code hooks: https://code.claude.com/docs/en/hooks
- Codex hooks: https://learn.chatgpt.com/docs/hooks
- `src/hooks.rs`, `src/events.rs`, `docs/lifecycle-integration.md`, ADR 0002/0004

## Comments

- 2026-10-15: Filed from the strategy review. Verified event names against the
  vendor docs; jjfx's current registration was read from `src/hooks.rs`.
