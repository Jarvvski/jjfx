# Spike safe Unix process and locking primitives

Status: ready-for-agent

## Parent

epics/B-worker-pool-and-runtime.md

## Problem Statement

wsg depends on Unix file locks, process groups, liveness checks, signals, detached children, and atomic rename. jjfx forbids unsafe Rust crate-wide. Selecting an adapter without proving these behaviors could invalidate Run supervision or force an unacceptable safety exception later.

## Solution

Build a throwaway, test-driven spike that demonstrates every required operating-system behavior through safe Rust interfaces. Record the chosen standard-library or dependency surface, platform limits, and failure modes. Do not connect the spike to production Worker Pool behavior.

## Commits

1. Specify the exact Unix behaviors and supported platforms inherited from Go wsg.
2. Prove exclusive advisory locking between two independent processes using the established lock-file names.
3. Prove a spawned command enters its own process group without unsafe application code.
4. Prove graceful group termination followed by forced termination removes descendants.
5. Prove PID liveness checks distinguish a running process from a reaped process.
6. Prove parent file handles can close without terminating or truncating child log output.
7. Prove same-directory temporary write, flush, close, and rename produces atomic reader-visible replacement.
8. Record findings, selected adapters, dependency implications, and unsupported cases.
9. Remove throwaway executable code while retaining focused reusable tests or findings that gate production work.

## Decision Document

- `unsafe_code = "forbid"` is non-negotiable for this effort.
- A safe wrapper crate is acceptable if the standard library does not expose the required operation.
- The initial platform contract is the Unix behavior wsg already supports.
- Process-group identity and PID reuse limitations must be documented.
- A failed graceful termination must have a bounded forced-termination path.

## Testing Decisions

Use subprocess integration tests with strict timeouts and guaranteed cleanup. Avoid mocks for signals and locks because the kernel behavior is the subject of the spike. Ensure failures cannot leave long-lived test children behind.

## Acceptance Criteria

- [ ] Every required primitive is demonstrated without application unsafe code.
- [ ] Cross-process locking is proven, not inferred.
- [ ] Descendant process cleanup is proven.
- [ ] Atomic replacement behavior is proven.
- [ ] Findings select a production path or explicitly block the migration.
- [ ] `mise run check` is green.

## Out of Scope

- Production Worker persistence
- Windows support
- Agent Runtime command construction
- TUI behavior

## Blocked by

- issues/02-create-shared-rust-foundation.md
