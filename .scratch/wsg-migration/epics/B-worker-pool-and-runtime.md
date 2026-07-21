# Epic B - Worker Pool and runtime

Type: epic
Status: tracking

## Goal

Reimplement the reusable Worker Pool and Run lifecycle in safe Rust while remaining interoperable with the Go process. This epic owns persistence, locking, Worker Workspace lifecycle, Agent Runtime execution, structured logs, Agent Sessions, and Worker actions.

## Entry preconditions

Epic A must be complete. In particular, the shared library and compatibility fixtures must exist before Rust mutates any wsg state.

## Execution order

1. **05-spike-safe-unix-primitives** proves safe adapters for process groups, signals, PID checks, locks, and atomic replacement.
2. **06-port-state-persistence-and-locking** implements the compatible state repositories and cross-process locks.
3. **07-port-workspace-and-pool-lifecycle** adds Worker Workspace provisioning and Worker Pool mutation.
4. **08-port-agent-runtime-and-run-supervision** adds Claude Code and Codex Run execution and reconciliation.
5. **09-port-logs-sessions-and-worker-actions** adds provider log interpretation, Agent Session continuation, and post-launch actions.

The Unix spike may run while Epic A finishes, but no production dependency may be selected before its findings are accepted.

## How to work it

- Preserve `unsafe_code = "forbid"`.
- Test through public state repositories and lifecycle interfaces.
- Exercise real cross-process exclusion, not only in-memory concurrency.
- Keep provider details behind Agent Runtime and log adapters.
- Preserve recoverability after every failed external command.
- Follow the landing gate in `CLAUDE.md` for every focused change.

## Definition of done

- Rust and Go serialize pool and Worker mutations through the same locks.
- Rust can create, grow, shrink, reset, and destroy compatible Worker Pools.
- Claude Code and Codex Runs launch in Worker Workspaces and finalize exactly once.
- Reset terminates the complete Agent Runtime process group.
- Logs, Agent Session IDs, Send, Review, Rebase, Open PR, Mount, and Reset work through shared interfaces.
- `mise run check` is green.
