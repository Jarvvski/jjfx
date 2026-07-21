# Port structured logs, Agent Sessions, and Worker actions

Status: ready-for-agent

## Parent

epics/B-worker-pool-and-runtime.md

## Problem Statement

Run supervision alone cannot determine provider result details, display current activity, continue an Agent Session, or implement the operational actions users rely on after a Dispatch. Claude Code and Codex emit different event shapes, and leaking those shapes to both frontends would duplicate parsing and behavior.

## Solution

Add provider-specific log adapters behind a shared event and result model. Use that model to finalize Runs, extract Agent Session IDs, summarize activity, and implement Send, Review, Reset, Rebase, Open PR, Logs, and Mount through a shared Worker actions interface.

## Commits

1. Define provider-neutral Run result, usage, activity, and collaboration event values.
2. Parse Claude Code stream events into the shared values.
3. Parse Codex events into the same values without erasing provider-specific diagnostics.
4. Implement bounded tail scanning for current activity and full result scanning for finalization.
5. Extract Agent Session identity with explicit fresh-session fallback reasons.
6. Connect structured Run results to exactly-once supervisor finalization.
7. Implement Send as a Follow-up that reuses the selected Agent Runtime and Session when possible.
8. Implement Review prompt construction from pull-request checks, review state, and merge state.
9. Implement Reset over process-group termination, state clearing, and asynchronous Workspace restoration.
10. Implement Rebase, Open PR, Logs, and Mount as typed Worker actions with frontend-neutral outcomes.

## Decision Document

- The shared library parses logs; frontends render events.
- Agent Session continuation is observable and reports whether it resumed or started fresh.
- A Follow-up is a new Run and can start on an idle Worker without a prior Session.
- Reset is the sole operation that abandons an active Run and returns a Worker to Idle.
- Provider collaboration events do not create extra Worker Pool slots.
- Mount remains a kitty adapter but is invoked through a typed action.

## Testing Decisions

Port behavioral scenarios, not Go parser structure. Use fixture lines for both providers, malformed and truncated logs, missing Session IDs, failed reviews, and action command failures. Test every action at its public interface with fake command adapters and temporary state.

## Acceptance Criteria

- [ ] Run finalization reads compatible provider results and costs.
- [ ] Current activity is available without scanning unbounded log data.
- [ ] Follow-up reports resumed versus fresh behavior.
- [ ] Worker actions return typed outcomes suitable for CLI and TUI rendering.
- [ ] Reset restores capacity and cleans the process group.
- [ ] `mise run check` is green.

## Out of Scope

- TUI log rendering
- Ticket discovery
- Dispatch Group progression
- New Agent Runtime providers

## Blocked by

- issues/08-port-agent-runtime-and-run-supervision.md
