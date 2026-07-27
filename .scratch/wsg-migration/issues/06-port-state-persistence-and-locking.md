# Port compatible state persistence and locking

Status: resolved

## Parent

epics/B-worker-pool-and-runtime.md

## Problem Statement

Read-only snapshots cannot safely become mutable until Rust writes exact compatible state and coordinates with concurrent Go processes. In-memory synchronization is insufficient because wsg commands, orchestrators, and TUI processes run independently.

## Solution

Implement deep state repositories for Worker Pool, Worker, and Dispatch Group persistence. Each repository owns path selection, lock acquisition, reload-under-lock, validation, atomic replacement, and compatible serialization. Callers operate on typed mutations rather than raw files.

## Commits

1. Implement atomic compatible writes against the golden fixtures.
2. Add a Worker state repository using the established per-Worker sidecar lock.
3. Add a Worker Pool repository using the established pool mutation lock.
4. Add a Dispatch Group repository with atomic replacement and compatible naming.
5. Reload current state after acquiring every mutation lock.
6. Preserve the last valid file when serialization, flush, or rename fails.
7. Add cross-process tests where independent Rust writers contend for each lock.
8. Add mixed Go and Rust lock tests using temporary pools and bounded subprocesses.
9. Replace read-only snapshot internals with repository reads without enabling new UI actions.

## Decision Document

- State repositories are the only modules allowed to know persistent file names and atomic-write mechanics.
- Mutations always reload under the lock.
- Lock files remain sidecars because atomic rename changes the state-file inode.
- Existing schemas remain unchanged.
- Errors retain Repository, Worker, and operation context.
- Readers never treat malformed state as empty valid state.

## Testing Decisions

Test repositories through public read and mutate operations. Cover missing files, malformed files, lock contention, concurrent reservations, interrupted writes, and exact round trips. Mixed-process tests are required because full transition compatibility is an explicit product requirement.

## Acceptance Criteria

- [x] Rust writes state accepted by current Go wsg.
- [x] Go reads Rust-created pool, Worker, and Dispatch Group files.
- [x] Concurrent Go and Rust mutations serialize through the same locks.
- [x] Every mutation reloads after lock acquisition.
- [x] Failed writes preserve the previous valid state.
- [x] `mise run check` is green.

## Comments

2026-07-27 - Go wsg commit `b85c8e8b24fdf5c5c39e7ceb6941cf045e8b3a10`
established the complete cooperating lock protocol, stale-write detection,
unknown-field retention, and failure-safe atomic replacement. Rust now exposes
opaque-revision Pool, Worker, and Dispatch Group repositories over the same
wire formats and lock order. Public repository tests, independent Rust writer
tests, and bidirectional Go/Rust subprocess conformance are green. jj-wsx was
removed from this ticket's active compatibility criteria because it is an
obsolete predecessor to jjfx and does not consume Worker Pool state.

## Out of Scope

- Deciding Worker lifecycle transitions
- Provisioning Workspaces
- Launching processes
- Schema redesign

## Blocked by

- issues/03-lock-down-compatibility-contracts.md
- issues/05-spike-safe-unix-primitives.md
