# Complete Pi Worker actions and release integration

Status: resolved

## Parent

pi-support effort (standalone; no PRD)

## Problem Statement

The Pi worker runtime core can identify, probe, invoke, parse, and supervise a
Pi Run, but Worker actions still assume only Claude and Codex. Follow-up/send
must resolve Pi sessions through the runtime-aware session contract, and
interactive mount must launch only a verified Pi command. The user-facing
runtime guidance and release metadata must also describe the trusted Worker
policy and Pi limitations without claiming unsupported budget, approval,
sandbox, or discovery capabilities.

## Solution

Finish the Pi Worker integration over the deep runtime interface delivered by
issue 02. Reuse the provider-neutral Worker Pool, reservation, Run Supervisor,
RunLog, and action outcomes. Keep Pi command details private to runtime and
interactive command adapters. A Pi Follow-up resumes only a proven Pi session;
otherwise it reports the existing explicit fresh-session reason. Interactive
mount either uses a verified fresh or resumed Pi form or returns a typed
unsupported outcome. It must never replace a failed Pi launch with an arbitrary
shell command.

Document that trusted Worker mode disables inherited Pi resources, uses the
selected coding-tool allowlist, and does not provide filesystem confinement.
Pi ticket discovery remains a separate capability and is implemented by issue
04 only when its configured read-only transport is available.

## Commits

1. Connect Pi to provider-neutral Follow-up/send session resolution and preserve
   runtime identity through reservation, Run, finalization, and fresh fallback.
2. Add verified Pi interactive Worker mount forms, shell quoting, executable
   validation, and typed unsupported behavior without changing Claude/Codex
   mount commands.
3. Add focused WorkerActions, fake executable, process cleanup, and compatible
   persistence coverage for Pi continuation and mount behavior.
4. Update runtime compatibility and setup guidance, bump the version according
   to repository policy, and add a dated changelog entry.
5. Run the complete repository verification gate and resolve any regressions
   without changing the selected Pi contract.

## Decision Document

- `AgentRuntime::Pi` remains separate from interactive `AgentKind::Pi`, which is
  owned by the lifecycle ticket.
- Follow-Up and mount select the persisted Worker runtime before interpreting a
  prior log. A Pi log is never resumed by Claude or Codex.
- Pi fresh and resumed commands use the validated runtime adapter. Prompt and
  session values remain direct arguments or correctly shell-quoted only at the
  kitty launch seam.
- Pi's trusted Worker policy is explicit. It suppresses inherited extensions,
  skills, prompt templates, themes, context files, and project trust, and uses
  the fixed coding-tool allowlist from issue 01. Pi's tool policy alone is not
  a filesystem sandbox.
- Pi aggregate budget limits and per-tool approval dialogs are unsupported
  until a separate capability is implemented. They must not be silently
  translated into Claude or Codex flags.
- Pi ticket discovery is not inferred from Worker execution and does not fall
  back to another runtime.

## Testing Decisions

Use public `WorkerActions`, `RunLog`, `RunSupervisor`, and mount outcomes as the
test seams. Use fake Pi and kitty executables to capture argv, cwd, environment,
exit status, and shell quoting. Use sanitized session and JSON fixtures. Cover
resumed and fresh continuation, missing or malformed identity, probe failure,
unsupported mount, spawn failure, cleanup, and unchanged Claude/Codex behavior.
Run `mise run check` after the implementation and do not run an unrestricted
TestContainers test command.

## Acceptance Criteria

- [x] Pi Follow-up reports resumed versus fresh behavior using runtime-aware
      session resolution and never resumes through Claude or Codex.
- [x] A missing, unreadable, malformed, identity-free, or incompatible Pi log
      returns an explicit fresh-session reason without mutating Worker state.
- [x] Pi interactive mount uses verified fresh and resumed forms or returns a
      typed unsupported result. It never masks a failed Pi launch with a shell.
- [x] Prompt, session, workspace, and tool-policy values are preserved without
      unsafe shell interpolation.
- [x] Background cleanup, PID persistence, Reset, liveness reconciliation, and
      stale finalization remain provider-neutral and green.
- [x] Claude and Codex Follow-up, mount, logs, and cleanup behavior remain
      unchanged.
- [x] Runtime compatibility, setup guidance, version, changelog, and
      `mise run check` are complete.

## Out of Scope

- Pi ticket discovery, Linear access, Direct Dispatch, Dispatch Group, and
  broad CLI/TUI runtime selection surfaces, which belong to issue 04.
- Pi interactive lifecycle events and `AgentKind::Pi`, which belong to issue 05.
- Live Pi credentials, live Linear access, or manual acceptance of the real
  installation, which belong to issue 06.
- Changing Claude or Codex command policy.

## Blocked by

- issues/01-spike-pi-contracts.md
- issues/02-add-pi-worker-runtime.md

## Comments

- 2026-08-14: Resolved with provider-aware `WorkerActions` profiles,
  runtime-aware Pi Follow-up, verified fresh and resumed Mount commands, and
  shared process cleanup coverage. Session resolution and failed preflight are
  non-mutating; a successfully launched fresh Follow-up uses the normal Worker
  lifecycle. Pi CLI/TUI profile selection remains in issue 04. `mise run check`
  passed for jjfx 0.36.0 and wsg 0.11.0.
