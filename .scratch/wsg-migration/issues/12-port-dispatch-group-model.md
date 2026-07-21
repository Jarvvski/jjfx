# Port the Dispatch Group dependency model

Status: ready-for-agent

## Parent

epics/C-dispatch-and-orchestration.md

## Problem Statement

Parent Tickets require a persistent dependency state machine that decides readiness, Dispatch Waves, retry eligibility, branch bases, and terminal completion. If these rules are mixed with live process and file operations, they become difficult to verify and easy to diverge from Go during coexistence.

## Solution

Implement a pure Dispatch Group aggregate over typed Sub-issue state. It computes Ready Sub-issues, maximum wave size, base branches, dependency context, status counts, retries, and terminal state. Persist through the compatible state repository but keep all live effects behind a narrow execution interface.

## Commits

1. Add typed Sub-issue Status and Dispatch Group values matching compatible wire states.
2. Build a Dispatch Group from a validated Parent Ticket dependency graph.
3. Implement Ready selection in stable Ticket order.
4. Implement maximum Dispatch Wave size for pool planning.
5. Implement terminal and status-count queries.
6. Implement base-branch and dependency context selection for Stacked Pull Requests.
7. Implement dispatched, done, failed, merged, and retry transitions.
8. Add invariants preventing unknown Workers, duplicate launches, and impossible self-dependencies.
9. Round-trip every Dispatch Group golden fixture.
10. Add a fake execution world for full state-machine tests without external commands.

## Decision Document

- Dispatch Group is the aggregate that owns dependency progression.
- Persistence is external to pure transition methods.
- A failed first Run is retryable once; a repeated failure is terminal.
- Merged Sub-issues satisfy downstream Dependencies.
- Branch base selection follows direct Blockers and preserves current stacked behavior.
- Stable ordering is part of predictable Dispatch outcomes.

## Testing Decisions

Test through aggregate methods and the fake execution seam. Cover chains, diamonds, independent Sub-issues, already merged work, malformed cycles, failed reset, exhausted retry, missing branches, and restart round trips. Do not assert private collection choices.

## Acceptance Criteria

- [ ] Every Go-created Dispatch Group fixture loads and round-trips.
- [ ] Ready and wave-size calculations match dependency expectations.
- [ ] Retry and terminal behavior is explicit and exhaustively tested.
- [ ] Base selection supports Stacked Pull Requests.
- [ ] The model performs no filesystem, process, Linear, or terminal I/O.
- [ ] `mise run check` is green.

## Out of Scope

- Watching live Workers
- Launching Runs
- CLI rendering
- General graph algorithms outside Dispatch Groups

## Blocked by

- issues/03-lock-down-compatibility-contracts.md
- issues/10-port-linear-discovery-and-prompts.md
