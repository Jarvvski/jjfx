# Adopt the Workspace Dispatch domain

Status: ready-for-agent

## Parent

epics/A-contract-and-coexistence.md

## Problem Statement

jjfx deliberately postponed Worker Pool orchestration in ADR 0001 and its glossary currently uses Agent and Workspace language without the execution-slot and Ticket concepts wsg needs. Implementing migration code before resolving that language would create overlapping meanings for Worker, Agent, Session, Run, and Workspace.

## Solution

Record the decision that the postponed orchestration layer is now being adopted. Supersede the limiting portion of ADR 0001 while preserving its history, and extend the domain glossary with the Workspace Dispatch terms from the migration PRD. Clarify that jjfx observes Agent lifecycle and also owns Worker execution lifecycle, which are related but not interchangeable.

## Commits

1. Add an ADR accepting Workspace Dispatch as a new layer over the existing Workspace and Agent models.
2. Mark ADR 0001 as superseded only where it says jjfx does not orchestrate, retaining its original rationale.
3. Extend the glossary with Worker Pool, Worker, Worker Workspace, Run, Agent Runtime, Ticket, Reservation, Dispatch, Direct Dispatch, Dispatch Group, and Dispatch Wave.
4. Add explicit distinctions between Worker, Agent, Agent Session, Workspace, and process.
5. Cross-reference the migration PRD and existing lifecycle ADRs.

## Decision Document

- jjfx now owns active orchestration as well as observation.
- The existing Agent and Work lifecycles remain orthogonal.
- Worker Status is a separate execution-capacity lifecycle.
- A Worker is backed by one Worker Workspace but is not itself a Workspace.
- A Run is one execution attempt; an Agent Session can span several Runs.
- ADR 0005 remains valid because the replacement is a native Rust implementation.
- ADR 0007 continues to govern jj-lib usage.

## Testing Decisions

This is a documentation-only architectural change. Validate terminology against the PRD and existing glossary, and verify all ADR references resolve. No runtime test is required.

## Acceptance Criteria

- [x] The accepted architecture explicitly permits Worker Pool orchestration.
- [x] New terms are defined once and avoid the glossary's rejected synonyms.
- [x] ADR 0001's historical decision remains readable and its superseded scope is clear.
- [x] ADR 0005 and ADR 0007 are not contradicted.
- [x] `mise run check` is green.

## Out of Scope

- Implementing any Rust module
- Changing current jjfx behavior
- Designing CLI flags or TUI keybindings

## Blocked by

Nothing - can start immediately.
