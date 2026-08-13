# Add Pi worker runtime, logs, sessions, and mount

Status: ready-for-agent

## Parent

pi-support effort (standalone; no PRD)

## Problem Statement

The shared worker library treats Agent Runtime as a closed Claude/Codex enum.
Runtime parsing, persisted pool identity, command construction, capability
probes, log parsing, session resolution, follow-ups, and interactive mount all
need a Pi adapter before a Pi worker can safely execute or continue work.

Pi's invocation and session contracts differ from both existing providers. A
Pi process may emit JSON events rather than Claude stream-json or Codex JSONL,
and Pi's session identity is represented by its session header and session
file. Reusing either existing parser or resume command would lose activity,
start a new session unexpectedly, or launch with the wrong tool and approval
policy.

## Solution

Add Pi as a first-class worker runtime behind the existing provider-neutral
interfaces. Use the contract and capability decisions from issue 01 to keep
Pi-specific command construction, probing, JSON DTOs, session identity, and
interactive mount details inside the shared library. Reuse the existing Run
Supervisor, Worker state transitions, follow-up reservation flow, and
provider-neutral activity/result values wherever the Pi contract permits it.

Legacy pools with missing or blank runtime configuration continue to default to
Claude. Existing Claude and Codex wire values, commands, logs, sessions, and
mount behavior remain unchanged. Pi capabilities that issue 01 marks
unsupported must be represented by typed errors or absent optional values, not
by a silent fallback to another runtime.

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
7. Connect Pi to Follow-up session resolution and interactive Worker mount,
   including fresh and resumed command forms, shell quoting, workspace policy,
   and executable validation.
8. Add focused unit and integration coverage using sanitized Pi fixtures and
   fake executables for invocation, probing, stream parsing, result handling,
   session continuation, mount, spawn failure, malformed output, and cleanup.
9. Update the runtime compatibility documentation and user-facing configuration
   guidance, bump the version according to the repository policy, and add a
   dated changelog entry for the new worker runtime.

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
- A Pi follow-up resumes only when the configured Pi session identity is
  proven. Otherwise it starts a fresh Pi session and exposes the explicit
  fallback reason already used by the shared Worker action API.
- Pi's permission, trust, and tool policy must be selected from issue 01's
  evidence. Claude or Codex flags must not be copied by analogy.

## Testing Decisions

Use fake Pi executables for deterministic argument, environment, exit-status,
stdout/stderr, and process-tree tests. Use sanitized fixture files for every
supported Pi JSON event and session entry, including unknown and malformed
records. Exercise public runtime, Run Supervisor, RunLog, session-resolution,
and WorkerActions seams rather than coupling tests to provider DTO structure.
Run `mise run check` after the implementation and do not run an unrestricted
TestContainers test command.

## Acceptance Criteria

- [ ] Claude and Codex runtime parsing, persistence, invocation, logs, session
      continuation, mount, and existing tests remain unchanged and green.
- [ ] Pi is accepted as a canonical configured and persisted runtime while
      missing or blank legacy configuration still selects Claude.
- [ ] Pi executable and capability failures identify the runtime and the
      missing capability without mutating Worker state.
- [ ] Fresh and resumed Pi commands match the issue 01 contract, preserve the
      workspace and safe tool policy, and never route prompt text through a
      shell.
- [ ] Pi JSON streams normalize into provider-neutral activity, result, usage,
      cost, failure, and session values without leaking Pi DTOs to callers.
- [ ] Current activity uses bounded log reading and terminal results use the
      shared full-log finalization path, with partial and unknown records
      handled according to the selected contract.
- [ ] Pi Follow-up reports resumed versus fresh behavior and never resumes a
      Pi session through Claude or Codex.
- [ ] Pi Worker mount supports verified fresh and resumed interactive forms or
      returns a typed unsupported outcome; it never opens an arbitrary shell
      command in place of a failed Pi command.
- [ ] Background cleanup, PID persistence, Reset, liveness reconciliation,
      and stale finalization remain provider-neutral and covered.
- [ ] Compatibility documentation, version, changelog, and `mise run check`
      are complete.

## Out of Scope

- Pi-based Linear ticket discovery or provider-specific Dispatch queries.
- Direct Dispatch, Dispatch Group orchestration, CLI selection surfaces, and
  jjfx worker TUI presentation beyond the shared library seams needed to carry
  the new runtime.
- Pi interactive lifecycle hooks and `AgentKind` display behavior.
- Live Pi credentials, live Linear access, or manual acceptance of the real
  installation.
- Changing Claude or Codex command policy to make it resemble Pi.

## Blocked by

- issues/01-spike-pi-contracts.md
