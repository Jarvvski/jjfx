# Add the Pi Direct Dispatch profile

Status: resolved

## Parent

pi-support effort (standalone; no PRD)

## Problem Statement

Read-only Pi ticket discovery does not make a Pi Worker capable of completing
the current Dispatch contract. Pi requires an explicit provider and model, and
the Dispatch prompt tells Workers to fetch, claim, update, and comment on a
Linear Ticket. Pi core has no native Linear tools, aggregate budget flag,
per-tool approval dialog, or inherited extension contract that jjfx can safely
assume.

## Solution

Carry the existing typed `AgentModel` through Dispatch prompt, Direct Dispatch,
and orchestration interfaces. Add an explicit Pi Linear extension profile that
preflights the required read/write direct tools and is selected deliberately
for Worker execution. Keep provider flags, extension details, and fixed trusted
Worker policy private to the Pi runtime adapter.

Reject missing provider/model, missing Linear profile, aggregate budget, and
unsupported approval/tool choices before launch and before persistent Worker
state is mutated. Preserve Claude and Codex behavior.

## Commits

1. Carry `AgentModel` through `DispatchPromptContext`,
   `DirectDispatchRequest`, and orchestration while preserving model-only
   convenience for Claude and Codex.
2. Define and preflight the explicit Pi Linear extension profile and required
   direct read/write Ticket tools.
3. Map Pi system prompt, delegation, budget, tool, trust, and approval
   capabilities to supported values or typed unsupported outcomes.
4. Connect the profile to foreground/background Direct Dispatch and preserve Pi
   runtime identity through reservation, Run, finalization, and Follow-up.
5. Add public-seam fake runtime/extension tests, setup guidance, versions, and
   changelog, then run the full verification gate.

## Decision Document

- `AgentModel` is the provider-neutral interface for provider plus model.
- Pi Direct Dispatch requires an explicit, preflighted Linear extension
  profile. Discovery helper configuration alone is insufficient.
- Pi does not inherit global or project extensions, skills, prompts, trust, or
  tools. The selected extension and exact direct tools are explicit.
- Missing capability never falls back to Claude/Codex or silently omits Linear
  delivery obligations.
- Pi aggregate budgets and per-tool approval dialogs remain unsupported.

## Testing Decisions

Test prompt behavior through `DispatchPromptBuilder`, execution through
`DirectDispatch`, and lifecycle behavior through provider-neutral Run and
Follow-up outcomes. Use fake Pi and extension executables; do not use live
credentials. Run vertical red-green cycles and `mise run check`.

## Acceptance Criteria

- [x] Dispatch callers can supply Pi provider and model through `AgentModel`.
- [x] Pi Direct Dispatch preflights an explicit Linear profile with the exact
      required read/write tools before reservation or launch.
- [x] Supported prompt/tool/trust choices are applied explicitly and unsupported
      budget/approval choices return typed errors.
- [x] Pi runtime identity remains stable through reservation, Run completion,
      failure, and Follow-up.
- [x] No inherited resources, provider fallback, credential leakage, or silent
      delivery-contract weakening occurs.
- [x] Claude and Codex prompt and Direct Dispatch behavior remains unchanged.
- [x] Documentation, versions, changelog, and `mise run check` are complete.

## Out of Scope

- Read-only discovery helper implementation, owned by issue 04.
- Dispatch Group and remaining CLI/TUI integration, owned by issue 08.
- Interactive lifecycle and manual acceptance, owned by issues 05 and 06.

## Blocked by

- issues/04-add-pi-ticket-discovery-and-dispatch.md

## Comments

- 2026-08-16 - Resolved with provider-aware Dispatch and orchestration models,
  pinned `pi-mcp-adapter` 2.11.0 package and direct-tool schema preflight,
  explicit Pi runtime policy, pre-mutation Direct Dispatch and Follow-up
  validation, public-seam fake runtime coverage, setup guidance, and the jjfx
  0.39.0 and wsg 0.13.0 release updates. `mise run check`, primary LSP
  diagnostics, `lens_diagnostics mode=all`, and `cargo fmt --check` completed;
  the lens report contains only pre-existing `CHANGELOG.md` Markdown warnings.
