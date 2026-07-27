# Port Agent Runtime invocation and Run supervision

Status: ready-for-agent

## Parent

epics/B-worker-pool-and-runtime.md

## Problem Statement

A reserved Worker is not useful until Rust can prepare and supervise Claude Code or Codex with the same foreground, background, logging, PID, and finalization semantics as Go. Launch races or incomplete process cleanup can strand Workers or leave child processes editing a reclaimed Workspace.

## Solution

Implement Agent Runtime adapters and one Run supervisor shared by initial Dispatch and Follow-up. The supervisor owns capability probing, command construction, process-group launch, PID persistence, foreground and background waiting, liveness reconciliation, and exactly-once terminal state.

## Commits

1. Add typed Agent Runtime selection with compatible defaulting and persisted identity.
2. Add executable availability and optional capability probes for Claude Code and Codex.
3. Build provider commands from typed invocation values without shell interpolation where direct execution is possible.
4. Add foreground Run execution with terminal passthrough and mirrored log output.
5. Add background process-group execution with child-owned log output.
6. Persist the PID before returning a successful launch outcome.
7. Add one waiter path that finalizes foreground and background Runs consistently.
8. Add liveness reconciliation for busy Workers whose recorded PID is gone.
9. Add graceful then forced process-group termination.
10. Guard finalization and Reset against concurrent attempts under the Worker lock.

## Decision Document

- Agent Runtime is distinct from Agent and Worker.
- One Run supervisor serves Dispatch and Follow-up.
- Provider capability probes are best effort and cannot block a Run when optional.
- Worker state records the Agent Runtime selected when the Run starts.
- Process cleanup targets the process group, not only the top-level PID.
- Finalization is idempotent and never overwrites a newer Run.

## Testing Decisions

Use fake executable scripts for deterministic provider argument, logging, exit-code, and signal tests. Add real process-group integration tests with child processes and strict cleanup. Test races between waiter, liveness reconciliation, and Reset through public Worker operations.

## Acceptance Criteria

- [ ] Claude Code and Codex commands preserve current invocation behavior.
- [ ] Foreground and background Runs produce compatible logs.
- [ ] Successful launch returns only after PID persistence.
- [ ] Dead busy Workers reconcile to done or failed exactly once.
- [ ] Reset terminates descendants and cannot finalize a later Run.
- [ ] `mise run check` is green.

## Out of Scope

- Parsing provider result events
- Agent Session continuation
- Linear queries
- Dispatch Group orchestration

## Blocked by

- issues/05-spike-safe-unix-primitives.md
- issues/06-port-state-persistence-and-locking.md
- issues/07-port-workspace-and-pool-lifecycle.md

## Comments

2026-07-27 - Started commit 1 through the public Worker Pool Reservation seam.
Reservations now resolve missing or blank pool runtime configuration to Claude,
normalize configured Claude/Codex values, reject unknown runtimes without
mutation, persist canonical runtime identity, and preserve existing Worker
fields and unknown extensions. Process launch, PID handling, and supervision
remain out of scope for this slice.

2026-07-27 - Added the shared Agent Runtime probe seam. Rust now reports
missing Claude or Codex executables as launch-blocking errors, runs optional
capability probes in the Worker Workspace, detects Claude forwarding and Codex
multi-agent support, and treats failed optional probes as unavailable
capabilities. Fake executable integration tests cover the public interface.

2026-07-27 - Added the typed Agent Runtime invocation seam. Claude and Codex
commands now preserve the source-validated headless, resume, model, workspace,
JSON, approval, and optional capability arguments without shell interpolation.
Fresh and resumed invocations are covered through the public command builder;
policy text remains caller-owned for ticket 10. Process launch, PID handling,
and supervision remain out of scope for this slice.

2026-07-27 - Added the provider-neutral foreground Run supervisor. Foreground
Runs now probe capabilities, execute in the Worker Workspace, inherit stdin,
mirror stdout and stderr to the terminal and one compatible truncated log, drain
both streams concurrently, reap the process, and return typed exit outcomes.
Coverage includes Claude and Codex fake runtimes, setup and spawn failures, log
truncation, and large dual-stream output. Background launch, process-group
cleanup, PID persistence, liveness reconciliation, and terminal finalization
remain pending.
