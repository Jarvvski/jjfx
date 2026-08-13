# Add Pi ticket discovery and Dispatch integration

Status: ready-for-agent

## Parent

pi-support effort (standalone; no PRD)

## Problem Statement

Worker runtime support alone does not make Pi selectable for the complete jjfx
workflow. Ready-ticket discovery currently builds provider-specific Claude and
Codex queries, and Direct Dispatch and Dispatch Group orchestration assume
those query and prompt capabilities. CLI configuration, runtime validation,
worker status, and the jjfx dispatch TUI also need to expose Pi without
leaking provider flags into domain decisions.

Pi has no built-in MCP. Its default tool set is local file and shell tooling,
and project-local resources may be subject to trust decisions. A discovery
implementation must therefore use the safe transport selected by issue 01,
constrain capabilities explicitly, and fail clearly when Linear access is not
configured. It must never silently use a different runtime, broaden Pi's
permissions, or reserve a Worker merely to discover tickets.

## Solution

Implement the Pi ticket-query adapter and connect it to the existing typed
TicketDiscovery, DispatchPrompt, Direct Dispatch, Dispatch Group, CLI, and
worker-TUI seams. Keep provider command flags and output DTOs private to the
adapter. Reuse existing ticket validation, retry, reservation, orchestration,
logging, and rendering behavior wherever Pi's capability contract permits it.

The selected discovery transport must be documented by issue 01. If that
transport requires a Pi extension, configuration, or external helper, missing
configuration must produce an actionable setup error and an explicit
unsupported-capability outcome. No discovery path may load untrusted project
resources or execute with workspace-write permissions unless the selected
workflow explicitly requires that policy and the contract documents it.

## Commits

1. Add Pi to the typed query/runtime capability selection used by ticket
   discovery while preserving the separate query-versus-Worker boundary.
2. Implement the issue 01-selected Pi discovery command or adapter with an
   explicit read-only policy, bounded working directory, trust behavior, and
   Linear access configuration.
3. Normalize Pi output into the existing constrained JSON payload, including
   sessionless/ephemeral behavior, JSONL or wrapper records, empty output,
   malformed output, stderr, exit status, and unknown fields.
4. Reuse the existing Ready Ticket and dependency-graph validation and retry
   policy for Pi, adding diagnostics that identify Pi and the missing setup
   without exposing credentials or raw private prompts.
5. Extend prompt capability mapping for Pi model, system prompt, delegation,
   budget, tool, and approval choices. Unsupported options must be rejected or
   represented explicitly rather than silently ignored.
6. Connect Pi discovery and prompt construction to Direct Dispatch and
   Dispatch Group orchestration, preserving reservation ordering, runtime
   identity, follow-up behavior, and provider-neutral progress/results.
7. Extend CLI runtime values, configuration, completions/help/error output,
   dispatch/session/log/action commands, and worker-TUI dispatch/detail views
   to display and select Pi consistently.
8. Add deterministic fake-query and fake-runtime tests for safe discovery,
   validation, retries, prompt capabilities, Direct Dispatch, Dispatch Group
   progression, CLI surfaces, and worker-TUI outcomes.
9. Update user-facing setup and capability documentation, bump the version
   according to repository policy, and add a dated changelog entry.

## Decision Document

- Ticket discovery remains separate from Worker reservation and Run execution.
- Pi's query transport is selected by the recorded issue 01 contract and is
  not inferred from Claude's MCP flags or Codex's command flags.
- Pi query runs are ephemeral or sessionless when the contract supports it;
  discovery must not pollute a user's interactive session history.
- Existing ticket graph validation remains authoritative. Pi output is input
  to the same typed validation, not a new provider-specific graph model.
- Prompt construction remains provider-neutral at its public boundary. Pi
  capability mapping owns only the translation and unsupported-option errors.
- A configured Pi runtime with unavailable ticket discovery is a visible
  capability error. It must not fall back to Claude/Codex discovery or mutate
  pool state.
- Direct Dispatch and Dispatch Group retain the selected Pi runtime through
  reservation, Run, finalization, follow-up, and TUI/CLI reporting.

## Testing Decisions

Use fake executables or injected query adapters to assert argv, environment,
working directory, read-only policy, output normalization, and failure
classification without live Linear credentials. Test provider-neutral
Dispatch and orchestration through public seams. Cover absent, blank, and
invalid Pi configuration; missing discovery setup; transient and permanent
failures; malformed JSON; duplicate and unsafe dependency graphs; and
unsupported model/budget/tool capabilities. Run `mise run check` after all
implementation slices.

## Acceptance Criteria

- [ ] Ready-ticket and dependency discovery can select Pi through the typed
      query interface without reserving a Worker.
- [ ] Pi discovery uses the issue 01-selected safe transport and explicit
      read-only/trust policy; it does not silently load untrusted project
      resources or broaden permissions.
- [ ] Pi output is normalized into existing Ticket and dependency values,
      including wrapper/JSONL output, empty output, malformed records, unknown
      fields, stderr, and non-zero exits.
- [ ] Existing validation and one-retry behavior apply to Pi, with actionable
      diagnostics that do not leak credentials or full private prompts.
- [ ] Missing Linear/discovery configuration returns an explicit setup or
      unsupported-capability result and never falls back to Claude or Codex.
- [ ] Prompt construction maps supported Pi model, system-prompt, delegation,
      budget, tool, and approval capabilities and reports unsupported choices.
- [ ] Direct Dispatch and Dispatch Group preserve Pi runtime identity from
      discovery through reservation, Run completion, follow-up, and result.
- [ ] CLI and worker-TUI surfaces list Pi accurately and display capability,
      progress, log, failure, and completion information without provider flag
      leakage.
- [ ] Claude and Codex discovery, Dispatch, orchestration, CLI, and TUI tests
      remain green and behaviorally unchanged.
- [ ] Documentation, version, changelog, and `mise run check` are complete.

## Out of Scope

- Adding or changing Pi's own MCP, extension, package, trust, or provider
  implementation outside the selected jjfx integration boundary.
- Interactive Pi lifecycle hooks, `AgentKind::Pi`, and lifecycle glyphs.
- Reworking provider-neutral Worker state, Run supervision, or log parsing
  already delivered by issue 02.
- Live Linear credentials or owner acceptance of a real Pi installation.
- Replacing the existing Claude/Codex ticket transports.

## Blocked by

- issues/01-spike-pi-contracts.md
- issues/03-complete-pi-worker-actions-and-release.md
