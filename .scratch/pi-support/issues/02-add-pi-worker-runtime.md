# Add the Pi worker runtime core

Status: claimed

## Parent

pi-support effort (standalone; no PRD)

## Problem Statement

The shared worker library treats Agent Runtime as a closed Claude/Codex enum.
Runtime parsing, persisted pool identity, command construction, capability
probes, structured log parsing, and session resolution need a Pi adapter before
a Pi Worker can safely execute or continue work. Follow-up actions and
interactive mount are tracked in the dependent continuation ticket.

Pi's invocation and session contracts differ from both existing providers. A
Pi process may emit JSON events rather than Claude stream-json or Codex JSONL,
and Pi's session identity is represented by its session header and session
file. Reusing either existing parser or resume command would lose activity,
start a new session unexpectedly, or launch with the wrong tool and approval
policy.

## Solution

Add Pi as a first-class Worker runtime behind the existing provider-neutral
interfaces. Use the contract and capability decisions from issue 01 to keep
Pi-specific command construction, probing, JSON DTOs, session identity, and
runtime-aware session resolution inside the shared library. Reuse the existing
Run Supervisor, Worker state transitions, and provider-neutral activity/result
values wherever the Pi contract permits it. Keep the runtime module deep: its
small public interface owns provider policy while private Pi adapters own CLI
and JSON details.

Legacy pools with missing or blank runtime configuration continue to default to
Claude. Existing Claude and Codex wire values, commands, logs, and sessions
remain unchanged. Pi capabilities that issue 01 marks unsupported must be
represented by typed errors or absent optional values, not by a silent fallback
to another runtime. This ticket does not add Pi discovery, interactive identity,
Follow-up actions, mount, CLI selection, versioning, or release documentation.

## Commits

1. Add `AgentRuntime::Pi`, canonical parsing/formatting, configured-runtime
   validation, persisted wire identity, and compatibility fixtures while
   preserving Claude defaulting and existing unknown-value errors.
2. Add Pi executable and capability probing based on the issue 01 contract,
   including clear not-found, failed-probe, unsupported-feature, and malformed
   capability outcomes.
3. Build fresh and resumed Pi worker commands from typed invocation values,
   including the selected JSON mode, prompt, model, system prompt, session,
   tool, trust, approval, and workspace policy without shell interpolation.
4. Add private Pi JSON stream DTOs and normalize session starts, assistant
   messages, thinking, tool activity, command results, failures, terminal
   completion, usage, cost, and unknown events into the existing public run
   log model.
5. Extract Pi session identity from the configured log/session contract and
   return explicit fresh-session reasons for missing, unreadable, malformed,
   or identity-free prior logs.
6. Connect Pi to provider-neutral foreground/background supervision and verify
   that log creation, PID persistence, finalization, process cleanup, and
   stale-run protection remain shared behavior.
7. Add focused unit and integration coverage using sanitized Pi fixtures and
   fake executables for runtime identity, probing, command construction, stream
   parsing, session resolution, spawn failure, malformed output, and cleanup.

## Decision Document

- `AgentRuntime::Pi` is persisted as the lowercase string `pi`; existing
  Claude/Codex strings remain byte-compatible where the wire contract requires
  it.
- Runtime identity is separate from interactive `AgentKind`; this ticket does
  not infer interactive hook state from worker logs.
- The existing Run Supervisor remains provider-neutral. Pi-specific behavior
  belongs in command, probe, log, and session adapters.
- Pi JSON records and session entries remain private implementation details.
  CLI, TUI, orchestration, and Worker actions consume existing normalized
  values.
- Runtime-aware session resolution is additive: existing provider-neutral
  resolution remains available while Pi callers select the runtime explicitly.
  A Pi session resumes only when its v3 session header identity is proven;
  malformed and unsupported records produce explicit fresh-session reasons.
- Pi command construction requires an explicit provider and model selection.
  Pi's trusted Worker mode suppresses inherited resources and uses the fixed
  coding-tool allowlist selected from issue 01. Pi's own tool policy is not a
  filesystem sandbox, so the limitation is visible to callers.
- Pi has no native aggregate budget or per-tool approval contract. A requested
  unsupported budget or approval override fails before process launch.
- Pi's permission, trust, and tool policy must be selected from issue 01's
  evidence. Claude or Codex flags must not be copied by analogy.

## Testing Decisions

Use fake Pi executables for deterministic argument, environment, exit-status,
stdout/stderr, and process-tree tests. Use sanitized fixture files for every
supported Pi JSON event and session entry, including unknown and malformed
records. Exercise public runtime, Run Supervisor, RunLog, and session-resolution seams
rather than coupling tests to provider DTO structure. WorkerActions coverage
belongs to the dependent continuation ticket. Run `mise run check` after the
implementation and do not run an unrestricted TestContainers test command.

## Acceptance Criteria

- [ ] Claude and Codex runtime parsing, persistence, invocation, logs, session
      continuation, and existing tests remain unchanged and green.
- [ ] Pi is accepted as a canonical configured and persisted runtime while
      missing or blank legacy configuration still selects Claude.
- [ ] Pi executable and capability failures identify the runtime and missing
      capability without mutating Worker state.
- [ ] Fresh and resumed Pi commands match the issue 01 contract, preserve the
      trusted Worker policy, require provider/model selection, and never route
      prompt text through a shell.
- [ ] Pi JSON streams normalize into provider-neutral activity, result, usage,
      cost, failure, and session values without leaking Pi DTOs to callers.
- [ ] Current activity uses bounded log reading and terminal results use the
      shared full-log finalization path, with partial and unknown records
      handled according to the selected contract.
- [ ] Runtime-aware Pi session resolution distinguishes valid identity,
      missing identity, unreadable logs, malformed records, and unsupported
      session versions without cross-provider fallback.
- [ ] Background cleanup, PID persistence, Reset, liveness reconciliation,
      and stale finalization remain provider-neutral and covered.
- [ ] Focused tests and `mise run check` are complete.

## Out of Scope

- Pi-based Linear ticket discovery or provider-specific Dispatch queries.
- Follow-up/send behavior and interactive Worker mount.
- Direct Dispatch, Dispatch Group orchestration, CLI selection surfaces, and
  jjfx Worker TUI presentation beyond the shared library seams needed to carry
  the new runtime.
- Pi interactive lifecycle hooks and `AgentKind` display behavior.
- Version bumps, changelog entries, and user-facing release guidance.
- Live Pi credentials, live Linear access, or manual acceptance of the real
  installation.
- Changing Claude or Codex command policy to make it resemble Pi.

## Blocked by

- issues/01-spike-pi-contracts.md
