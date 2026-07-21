# Prove parity and release wsg from jjfx

Status: ready-for-agent

## Parent

epics/D-interfaces-and-cutover.md

## Problem Statement

Passing unit tests does not prove the Rust binary can replace Go wsg. The migration specifically promises mixed-process safety, exact wire compatibility, process cleanup, restart recovery, CLI behavior, and release of two independently named binaries from one repository.

## Solution

Build an end-to-end conformance suite that treats current Go wsg as a temporary oracle and compatibility peer. Exercise both implementations in alternating order against temporary Repositories, then package and release jjfx and wsg together without overwriting the installed Go binary until owner validation.

## Commits

1. Add a scenario harness that can invoke explicitly selected Go and Rust wsg binaries.
2. Compare Workspace and Worker Pool command outcomes over the same temporary Repository.
3. Alternate Go and Rust pool growth, Reservation, Reset, and teardown under contention.
4. Launch fake Agent Runtime process trees from each implementation and reconcile them from the other.
5. Create Dispatch Group progress with one implementation and resume it with the other.
6. Compare CLI stdout, stderr roles, aliases, completion, and exit outcomes against the contract inventory.
7. Add restart and interrupted-write scenarios for Worker and Dispatch Group state.
8. Add release packaging for both binary names and their independent version metadata.
9. Update install tasks and release artifacts to install both binaries from jjfx.
10. Run a manual acceptance matrix using real Claude Code, Codex, Linear, gh, jj, kitty, and an existing Worker Pool.
11. Record cutover evidence and remaining known differences.

## Decision Document

- Go is an oracle only during migration and is not required after cutover.
- Semantic terminal output parity is sufficient where ANSI bytes are not consumed.
- Persisted state, lock behavior, exit status, and machine-readable output require strict compatibility.
- Release artifacts carry both binaries from the jjfx repository.
- Installation does not switch the user's default wsg until manual acceptance passes.

## Testing Decisions

Use temporary Repositories and fake external executables for automated conformance, with bounded timeouts and cleanup. Reserve live provider and external-service checks for a documented manual matrix. Test both implementation orders to catch one-way compatibility.

## Acceptance Criteria

- [ ] Go-created state is fully operable from Rust and Rust-created state from Go.
- [ ] Mixed concurrent processes do not double-reserve or corrupt files.
- [ ] Process groups are cleaned across implementation boundaries.
- [ ] Dispatch Group restart recovery works in both directions.
- [ ] Release artifacts contain working jjfx and wsg binaries.
- [ ] Manual acceptance passes before installation cutover.
- [ ] `mise run check` is green.

## Out of Scope

- Deprecation messaging in the Go repository
- Deleting old releases
- Supporting incompatible historical schemas not accepted by current Go wsg

## Blocked by

- issues/15-restore-wsg-dispatch-and-session-cli.md
- issues/16-integrate-dispatch-into-jjfx.md
