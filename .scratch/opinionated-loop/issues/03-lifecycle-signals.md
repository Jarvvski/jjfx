# Ingest the agent lifecycle signals jjfx ignores

Status: resolved
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

- [x] `Notification:agent_needs_input` produces `NeedsAttention`.
- [x] A rate-limit/billing `StopFailure` produces a distinct quota-blocked
      outcome, shown in the list, and is not retried immediately.
- [x] Subagent/`PostToolBatch` events are recorded and do not regress existing
      Working/Waiting transitions.
- [x] The event log stays backward compatible for existing consumers.
- [x] `mise run check` is green.

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
- 2026-10-15: Resolved. Registered `Notification`, `StopFailure`,
  `SubagentStart`/`Stop`, `PostToolBatch`, `TaskCreated`/`Completed`, and
  `TeammateIdle` for Claude and `Interrupt` + the subagent events for Codex. The
  shared event schema gained optional `notification_type`/`error` fields (old
  lines still parse). `Notification:agent_needs_input` folds to
  `NeedsAttention`; `StopFailure:rate_limit`/`billing_error` folds to a new
  `AgentState::QuotaBlocked` that sorts to needs-you with its own `⊘` glyph.
  Orchestration now parks a quota-blocked Run (a new
  `DispatchGroupEvent::QuotaBlocked` that ignores the retry allowance) instead
  of retrying it, driven by the hook-derived agent state for the Worker's
  workspace. Subagent/batch events are recorded without regressing
  Working/Waiting. `mise run check` green.

## Resolution

Reconciled Mon 19 Oct 2026 against the merged code at `9d6bd0ce6e7b`.

This ticket was marked resolved without its acceptance boxes ticked. Every criterion was re-verified by an independent read-only review of the code and tests on `main`. The reconciliation recorded no new work; it confirms what shipped.

Evidence: `src/hooks.rs:30-43` registers `Notification`, `StopFailure`, `SubagentStart`/`Stop`, `PostToolBatch`, `TaskCreated`/`Completed` and `TeammateIdle` (plus Codex's subagent events, `:57`). The shared `Event` struct carries the new optional `notification_type` field (`src/agent.rs:117`), and `notification_transition` (`:181`) folds `agent_needs_input` to `NeedsAttention`. `StopFailure:rate_limit`/`billing_error` folds to `AgentState::QuotaBlocked`, which has its own `⊘` glyph (`src/render/style.rs:204`) and sorts to needs-you. Orchestration parks a quota-blocked Run via `DispatchGroupEvent::QuotaBlocked` (`src/engine/orchestration.rs:483`, `src/engine/dispatch_group.rs:470`) rather than retrying it. Tests: `events::tests::a_fake_hook_emitter_drives_the_lifecycle_and_attention_through_the_log` (`src/events.rs:436`) and `a_fake_emitter_billing_failure_also_blocks_the_run` (`:504`); `agent::tests::other_notification_types_do_not_disturb_the_state` (`src/agent.rs:531`). Criterion 4 (backward compatibility) holds because the added fields are optional and an old line without them still parses - pinned by `agent::tests::parses_common_fields_and_ignores_the_rest` (`src/agent.rs:302`).

Not ticked: the `mise run check` criterion. It is a landing-commit property, and this reconciliation deliberately does not run the full suite (the orchestrator runs the gate once at the end).

Gate verified by the orchestrator on `df05f7c9aaef`: `mise run check` is green (changelog validation, fmt, clippy with warnings as errors, build and the full test suite), with no leaked test processes afterwards. The reconciliation above left this box unticked only because it ran no suite.
