# Spike safe Unix process and locking primitives

Status: resolved

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

- [x] Every required primitive is demonstrated without application unsafe code.
- [x] Cross-process locking is proven, not inferred.
- [x] Descendant process cleanup is proven.
- [x] Atomic replacement behavior is proven.
- [x] Findings select a production path or explicitly block the migration.
- [x] `mise run check` is green.

## Out of Scope

- Production Worker persistence
- Windows support
- Agent Runtime command construction
- TUI behavior

## Blocked by

- issues/02-create-shared-rust-foundation.md

## Answer

Resolved on 2026-07-21 with a Unix-only subprocess conformance suite in
`crates/wsg-core/tests/unix_primitives.rs`. The suite uses real kernel behavior,
strict waits, and drop cleanup rather than mocks.

### Selected adapters

- `std::os::unix::process::CommandExt::process_group(0)` safely makes the
  spawned child the leader of a new process group without application unsafe
  code.
- rustix 1.x with its `fs` and `process` features supplies safe `flock(2)`,
  signal-0 liveness probes, and process-group `SIGTERM`/`SIGKILL` delivery.
- Owned `std::fs::File` handles passed through `Stdio` let a child continue
  writing after parent-side handles close.
- `tempfile::NamedTempFile` creates the replacement beside its target. The
  selected sequence writes, flushes, syncs, closes through `into_temp_path`,
  then persists with an atomic same-directory rename.

These are concrete safe adapters, not a new Workspace Dispatch seam. Ticket 06
can hide locking and replacement behind the deep state-repository interfaces
already planned, while ticket 08 can hide process-group mechanics behind the
Agent Runtime interface.

### Proven behavior

- Independent processes contend on the established
  `.jj/pool/.dispatch.lock` and `.jj/pool/<worker>.json.lock` sidecars; a second
  process is excluded until the holder exits.
- The spawned leader's process-group ID equals its PID.
- Group termination gives the process group a bounded graceful window, then
  force kills a stubborn descendant and reaps its stubborn leader.
- A running PID passes signal-0 probing and the same PID fails after the child
  is reaped.
- A child writes complete delayed output after the parent closes its log
  handles.
- Concurrent readers observe only the complete old or complete new state
  across same-directory replacement.

### Source validation and limits

Go wsg commit `e690262ee0f9040f371ed1be9792742045af89e3` confirms the two lock
names above, `Setpgid`, signal-0 probing, group `SIGTERM` followed after one
second by `SIGKILL`, child-owned shell log redirection, and sibling `.tmp`
rename. Its Dispatch Group writer currently has no lock, so ticket 06 must
coordinate a new sidecar name with Go wsg before mixed-process Dispatch Group
mutation is enabled.

The selected path supports Linux and macOS Unix targets. Windows, network
filesystem lock semantics, PID-reuse-proof identity, and crash durability
beyond syncing the temporary file are unsupported. Numeric PIDs are therefore
liveness hints only, and callers must combine them with persisted Run identity.
