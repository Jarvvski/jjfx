# Epic C - Dispatch and orchestration

Type: epic
Status: tracking

## Goal

Reimplement Ticket discovery, Direct Dispatch, dependency-aware Dispatch Groups, and restart-safe orchestration over the Rust Worker Pool.

## Entry preconditions

Epic B must provide compatible Worker Pool mutation, Agent Runtime execution, and Run reconciliation. The Dispatch Group wire fixtures from Epic A must already be enforced.

## Execution order

1. **10-port-linear-discovery-and-prompts** provides Ready Ticket and dependency discovery plus provider-neutral prompt construction.
2. **11-port-direct-dispatch** adds Reservations, explicit Worker selection, pool growth, Workspace preparation, and Run launch.
3. **12-port-dispatch-group-model** adds the pure dependency state machine and compatible persistence.
4. **13-port-orchestration-runner** drives that state machine against live Workers with retries and restart recovery.

Tickets 10 and 12 can proceed independently once their prerequisites are available. Ticket 13 requires both Direct Dispatch and the Dispatch Group model.

## How to work it

- Keep Dispatch Group decisions pure and testable through a fake execution seam.
- Keep Linear and Agent Runtime transport details outside the dependency model.
- Persist every state transition before sleeping or returning control.
- Release Reservations on every pre-launch failure path.
- Preserve existing branch base selection for Stacked Pull Requests.
- Follow the landing gate in `CLAUDE.md` for every focused change.

## Definition of done

- Rust can discover Ready Tickets and Parent Ticket dependencies through Claude Code or Codex.
- Direct Dispatch is atomic with concurrent pool mutation.
- Existing Go-created Dispatch Groups round-trip through Rust unchanged.
- Dispatch Waves progress in dependency order and retry a failed Run once.
- A restarted orchestrator resumes persisted progress without duplicate launches.
- `mise run check` is green.
