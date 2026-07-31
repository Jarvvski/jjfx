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

## Comments

2026-07-31 - Started ticket 09 with its first focused slice. `wsg-core` now
exposes provider-neutral Run results, normalized token usage, exact micro-USD
costs, structured activity, and collaboration values through one public seam.
Provider DTOs, parsing, bounded scanning, Session resolution, finalization, and
Worker actions remain in the later slices of this ticket.

2026-07-31 - Completed the Claude Code parser slice through the public
`RunLogParser` seam. Private provider DTOs now normalize session starts,
ordered assistant messages, token usage, correlated tool lifecycles, legacy
tool completions, and terminal results with duration, turn count, failure
context, and micro-USD cost. Unknown well-formed events remain forward
compatible while malformed JSON returns a typed parse error.

Codex parsing remains the next focused slice. Bounded tail and final-result
scanning, Agent Session extraction, supervisor finalization, and Worker actions
remain later work in this ticket.

2026-07-31 - Completed the Codex parser slice through the same public
`RunLogParser` seam. Private Codex DTOs now normalize session starts, narrative
items, command and MCP diagnostics, web searches, file changes, plans, warnings,
collaboration state, all reported token counters, and terminal results. Unknown
well-formed events and item kinds remain forward compatible while malformed
JSON returns a typed parse error.

Bounded tail and final-result scanning is the next focused slice. Agent Session
identity, supervisor finalization, and Worker actions remain later work in this
ticket.

2026-07-31 - Completed bounded activity and full result scanning through the
public `RunLog` facade. Current activity reads only the final 65,536 bytes,
discards a split leading record, tolerates malformed and partially written log
records, and returns the latest provider-neutral activity. Final result scanning
streams the complete log, selects the latest terminal result, and preserves
Claude duration, turns, cost, and failure details plus all Codex usage counters.
Missing or unreadable logs and semantic provider failures retain path-aware typed
errors, while a readable log without a meaningful activity or result returns
`None`.

Agent Session identity with explicit fresh-session fallback reasons is the next
focused slice. Supervisor finalization and Worker actions remain later work in
this ticket.

2026-07-31 - Completed Agent Session identity resolution through the public
`resolve_agent_session` seam. The shared library now scans prior Run logs without
requiring persisted provider metadata, recognizes Claude Session IDs and Codex
thread IDs, skips malformed and partial records, and returns the first valid
identity. Missing, empty, unreadable, and identity-free logs produce explicit
typed reasons for starting a fresh Session instead of failing a Follow-up.

Connecting structured Run results to exactly-once supervisor finalization is the
next focused slice. Follow-up launch and Worker actions remain later work in this
ticket.
