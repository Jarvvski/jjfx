# Port Linear Ticket discovery and prompt construction

Status: ready-for-agent

## Parent

epics/C-dispatch-and-orchestration.md

## Problem Statement

wsg discovers Ready Tickets and Parent Ticket dependencies through the selected Agent Runtime's Linear MCP access. The workflow includes transient retry, malformed-response validation, repository identity, delivery instructions, and delegation constraints. Provider or terminal details must not leak into Dispatch decisions.

## Solution

Implement a Ticket discovery adapter and typed prompt builder in the shared library. Query through Claude Code or Codex, parse a constrained response, validate Parent Ticket and Sub-issue relationships, retry transient failure once, and produce provider-neutral Ticket and dependency values.

## Commits

1. Add Ticket ID, title, status, Parent Ticket, Blocker, and dependency graph value types.
2. Add a short-lived Agent Runtime query interface separate from a Worker Run.
3. Implement Ready Ticket discovery with configurable label and expected workflow state.
4. Implement Parent Ticket child and dependency discovery.
5. Validate duplicate children, parent-as-child, missing titles, self-blockers, unknown Blockers, and malformed statuses.
6. Add one bounded retry for transient query or parse failure.
7. Build initial Dispatch prompts from Repository identity, Ticket, delivery contract, model, and budget.
8. Build Follow-up delegation rules consistently across fresh and resumed Agent Sessions.
9. Preserve provider capability choices without exposing command-line flags to callers.

## Decision Document

- A Ticket is a Linear work item even though discovery is transported through an Agent Runtime.
- Discovery is separate from Run execution and does not consume a Worker.
- Invalid entries are reported and excluded when safe; an unusable graph fails as a whole.
- Prompt construction is typed and testable without launching a provider.
- Provider-owned model and token behavior remains provider-owned unless the caller supplies a supported override.

## Testing Decisions

Use deterministic query adapters returning valid, malformed, and transiently failing responses. Assert validated Ticket values and prompt semantics, not exact prose wrapping. Cover Claude Code and Codex selection without requiring live Linear credentials.

## Acceptance Criteria

- [ ] Ready Tickets are discovered through either configured Agent Runtime.
- [ ] Parent Ticket dependency graphs reject unsafe relationships.
- [ ] One transient failure retries and a persistent failure surfaces context.
- [ ] Prompt tests pin delivery and delegation obligations.
- [ ] No Worker is reserved during discovery.
- [ ] `mise run check` is green.

## Out of Scope

- Direct Dispatch
- Dispatch Group progression
- Live Linear integration tests requiring credentials
- General-purpose Linear client functionality

## Blocked by

- issues/08-port-agent-runtime-and-run-supervision.md
