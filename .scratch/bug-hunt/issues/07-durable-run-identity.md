# Give every Run a durable identity token

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

A Worker's recorded PID is the only thing jjfx knows about a live Run, and it is
used directly as a signal target:

- `reset_run` probes `live_process_group` and signals the recorded pid
  (`src/engine/runtime.rs:586-598`).
- `terminate_recorded_process` signals a pid read straight from Worker state
  (`:610-623`), used by Pool destroy.

The OS recycles pids. After a Run finishes, a later Reset or destroy can send
SIGTERM/SIGKILL to an **unrelated** process group. The same weak identity makes
`reconcile_worker` (`pool.rs:350-389`) keep a Worker `busy` forever when a
recycled pid happens to be alive (C7).

The compatibility contract calls PIDs "liveness hints, not Run identity"
(`docs/wsg-compatibility-contract.md:147`) - so the fix must add identity, not
repurpose `pid`.

## Fix

Record a durable per-Run identity alongside the pid and verify it before
signalling:

- Store a process start time (or a per-Run nonce written to a jjfx-owned
  location, like `.jj/pool/in-use.json` is) when the pid is persisted.
- Before TERM/KILL, confirm the target still matches the recorded identity;
  otherwise treat the process as already gone.
- Use the same identity in `reconcile_worker` so a recycled pid cannot keep a
  Worker busy.

Keep the identity out of the frozen Worker document (ADR-0011 precedent: the
in-use marker is jjfx-owned and ignored by a compatibility peer).

## Commits

1. Add the identity to the PID persistence path (`pool.rs`) and a jjfx-owned
   sidecar.
2. Verify identity in `reset_run` / `terminate_recorded_process` /
   `reconcile_worker`.
3. Tests: a forged/recycled pid is not signalled; a live Run is still
   terminated.
4. CHANGELOG entry `(fix)`.

## Decision Document

- Where the identity lives (sidecar vs. extension field) - keep the Worker
  document Go-compatible.
- macOS/Linux portability of the start-time probe (no `unsafe`; the crate
  forbids it).

## Acceptance Criteria

- [ ] A recorded pid that has been recycled is never signalled.
- [ ] A genuinely live Run is still terminated on Reset/destroy.
- [ ] A recycled pid no longer strands a Worker in `busy`.
- [ ] No new field on the Go-compatible Worker document.
- [ ] `mise run check` is green.

## Out of Scope

- Changing the contract's "liveness hint" wording.
