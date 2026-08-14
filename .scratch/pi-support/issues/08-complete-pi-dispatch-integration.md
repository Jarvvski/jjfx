# Complete Pi Dispatch integration

Status: ready-for-agent

## Parent

pi-support effort (standalone; no PRD)

## Problem Statement

Pi read-only discovery and Direct Dispatch profiles must be connected across
persistent Dispatch Groups, shared CLI commands, completions, Worker actions,
and jjfx TUI progress/detail views before Pi supports the complete Workspace
Dispatch workflow.

## Solution

Reuse the provider-neutral Dispatch Group, orchestration, Worker state, Run log,
and presentation models. Carry Pi runtime/profile identity through existing
interfaces and keep all Pi command and extension details inside the adapters
implemented by issues 04 and 07. Surface capability and setup failures without
provider fallback or raw configuration leakage.

## Commits

1. Connect Pi discovery and Direct Dispatch profiles to persistent Dispatch
   Group orchestration and restart/progression paths.
2. Extend shared CLI runtime/model/profile values, help, completions, errors,
   dispatch, session, log, and action output for Pi.
3. Extend jjfx Worker dispatch/detail/progress views for Pi capability,
   execution, failure, and completion outcomes.
4. Add deterministic Dispatch Group, CLI, and TUI regression coverage.
5. Update setup/release guidance, versions, and changelog, then run the full
   verification gate.

## Decision Document

- Dispatch Group remains runtime-neutral; runtime/profile identity belongs to
  reservations and Runs.
- Pi-specific flags and extension configuration never enter graph state or
  presentation values.
- Explicitly selected Pi never degrades to the legacy missing-runtime Claude
  default.
- Missing setup is a capability error, not an empty Ticket list or idle Worker.

## Testing Decisions

Test orchestration through `OrchestrationRunner`, CLI behavior through the
shared command interface, and rendering through jjfx TUI outcomes. Use fake
helper/runtime/extension executables and persisted fixtures. Run vertical
red-green cycles and `mise run check`.

## Acceptance Criteria

- [ ] Dispatch Group discovery, reservation, restart, progression, retry,
      completion, and failure preserve Pi identity.
- [ ] Shared CLI values, help, completions, errors, logs, sessions, and actions
      select and display Pi consistently.
- [ ] jjfx TUI views show Pi capability, progress, activity, failure, and
      completion without provider-detail leakage.
- [ ] Missing configuration never mutates Pool state or falls back to another
      runtime.
- [ ] Claude and Codex orchestration, CLI, and TUI behavior remains unchanged.
- [ ] Documentation, versions, changelog, and `mise run check` are complete.

## Out of Scope

- Discovery helper and Direct Dispatch profile implementation, owned by issues
  04 and 07.
- Interactive lifecycle and manual acceptance, owned by issues 05 and 06.

## Blocked by

- issues/04-add-pi-ticket-discovery-and-dispatch.md
- issues/07-add-pi-direct-dispatch-profile.md
