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
