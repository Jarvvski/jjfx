# Prove Go/Rust conformance for wsg

Status: ready-for-agent

## Parent

epics/D-interfaces-and-cutover.md

## Problem Statement

Passing unit tests does not prove the Rust binary can replace Go wsg. The
migration promises mixed-process safety, exact wire compatibility, process
cleanup, restart recovery, CLI behavior, and a compatibility peer that can be
run in either order. These behaviors need an automated conformance suite before
release packaging or cutover work begins.

## Solution

Build a two-layer end-to-end conformance suite that treats current Go wsg as a
temporary oracle and compatibility peer. The public layer invokes explicitly
selected Go and Rust `wsg` executables against temporary Repositories. A
private deterministic helper layer coordinates lock barriers, interrupted
writes, fake Agent Runtime process trees, and bounded cleanup where public CLI
inputs alone cannot make a race reproducible.

The dedicated conformance task requires explicit paths to the Go CLI and Go
test-helper binaries. Ordinary `mise run check` remains self-contained and
must not silently claim parity when the oracle is unavailable.

## Commits

1. Add a test-only scenario harness that can invoke explicitly selected Go and
   Rust wsg binaries and private deterministic helpers.
2. Compare Workspace and Worker Pool command outcomes over the same temporary
   Repository in both implementation orders.
3. Alternate Go and Rust pool growth, Reservation, Reset, resize, and teardown
   under deterministic contention barriers.
4. Launch fake Agent Runtime process trees from each implementation and
   reconcile, reset, and tear them down from the other.
5. Create Dispatch Group progress with one implementation and resume it with
   the other, including dependency waves and persisted assignments.
6. Compare CLI stdout, stderr roles, aliases, completion, and exit outcomes
   against the compatibility contract inventory.
7. Add restart and interrupted-write scenarios for Worker and Dispatch Group
   state, then document accepted differences and the conformance invocation.

## Decision Document

- Go is an oracle only during migration and is not required after cutover.
- The public executable boundary is the primary conformance interface.
- Private helpers are allowed only for deterministic coordination and cleanup;
  direct wsg-core tests do not replace public CLI scenarios.
- Persisted state, lock behavior, atomic replacement, and machine-readable
  output require strict compatibility.
- Human terminal output may use contract-aware semantic normalization rather
  than byte equality.
- CLI compatibility requires matching success/failure outcomes and stdout /
  stderr roles. Intentional behavior differences must be recorded rather than
  hidden.
- Conformance targets Unix because the shared process contract uses process
  groups and flock.
- Go oracle paths are explicit: `WSG_GO_BINARY` selects the public Go binary
  and `WSG_GO_TEST_BINARY` selects its helper test binary.

## Testing Decisions

Use temporary Repositories and fake external executables for automated
conformance, with bounded timeouts and process-group cleanup. Run every
cross-implementation scenario in both directions. Require the explicit Go
binaries in `mise run conformance`; a missing oracle is an actionable setup
failure, never a passing skip. Reserve live provider and external-service
checks for the follow-up manual acceptance ticket.

## Acceptance Criteria

- [ ] The conformance harness invokes selected Go and Rust binaries through a
      reusable test-only seam.
- [ ] Workspace, Worker Pool, Reservation, Reset, resize, teardown, and
      metadata scenarios pass in both implementation orders.
- [ ] Mixed concurrent operations do not double-reserve, deadlock, lose state,
      or leave malformed JSON or mismatched Workspace membership.
- [ ] Runtime process leaders and descendants are cleaned across the
      implementation boundary.
- [ ] Dispatch Group progress resumes across implementations without duplicate
      assignments and reaches matching terminal counts.
- [ ] CLI aliases, completion, stdout/stderr roles, and exit outcomes are
      covered with strict versus semantic comparison applied explicitly.
- [ ] Restart and interrupted-write scenarios preserve valid old-or-new state,
      unknown fields, and stale-revision protections.
- [ ] `mise run conformance` requires and records explicit Go oracle paths.
- [ ] `mise run check` is green.

## Out of Scope

- Release archive packaging or independent release/tag identity
- Installing candidate binaries or changing the user's installed Go binary
- Live Claude Code, Codex, Linear, gh, kitty, or existing-pool acceptance
- Deprecation messaging in the Go repository
- Supporting incompatible historical schemas not accepted by current Go wsg

## Blocked by

- issues/15-restore-wsg-dispatch-and-session-cli.md
- issues/16-integrate-dispatch-into-jjfx.md
