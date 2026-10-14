# Give every Run a durable identity token

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

A Worker's recorded PID is the only thing jjfx knows about a live Run, and it is
used directly as a signal target:

- `reset_run` probes `live_process_group` and signals the recorded pid
  (`src/engine/runtime.rs`).
- `terminate_recorded_process` signals a pid read straight from Worker state,
  used by Pool destroy.
- `reconcile_worker` (`src/engine/pool.rs`) uses `process_is_alive_i64(pid)` - a
  plain process-liveness probe - to decide whether a Run is still live.

The OS recycles pids. After a Run finishes, a later Reset or destroy can send
SIGTERM/SIGKILL to an **unrelated** process group; and a recycled pid that is
alive keeps a Worker `busy` forever (C7).

The compatibility contract calls PIDs "liveness hints, not Run identity"
(`docs/wsg-compatibility-contract.md`), so the fix must add identity, not
repurpose `pid`.

## Analysis (2026-10-13)

**The current code is safer than it first looks.** Both kill paths go through
`test_kill_process_group` / `kill_process_group`, so they only ever signal a
*process group*. A recycled pid that is not a group leader has no group with
that id, so the signal returns `ESRCH` and nothing is killed. The residual
hazard is a recycled pid that also happens to lead a process group.

**A complete fix needs a decision.** A durable identity (pid + process start
time, or a per-Run nonce the child carries) requires reading another process's
start time or environment. That is not reachable safely on macOS:

- `/proc/<pid>/stat` works on Linux only.
- `sysctl(KERN_PROCARGS2)` / `proc_pidinfo` need `unsafe`, which the crate
  forbids (`unsafe_code = "forbid"`).
- Shelling out to `ps -o lstart=` is portable but brittle to parse.

So a full fix needs either a new dependency (e.g. `sysinfo`) or an owner
decision to allow a narrow `unsafe`/FFI shim. **Neither is an agent call.**

## Decision Needed

1. Accept a new dependency (or a documented `unsafe` exception) for a real
   process-start-time identity, or
2. Accept the residual risk and document it, or
3. Add a lightweight, portable guard now: before signalling, require the
   recorded pid to be a live process-group leader *and* have `reconcile_worker`
   use the same group-based liveness.

## Note on option 3

Group-based liveness is only sound once **every** Run is its own process group.
Background Runs are (`process_group(0)`); foreground Runs are not, deliberately,
because an interactive foreground Run must stay in the terminal's foreground
process group to read input (C4). Changing that is itself a design decision about
foreground Runs, so C7 is coupled to C4 (ticket 20).

## Acceptance Criteria

- [ ] The owner picks option 1, 2, or 3.
- [ ] If 1 or 3: a recorded pid that has been recycled is never signalled, and a
      recycled pid no longer strands a Worker in `busy`.
- [ ] No new field on the Go-compatible Worker document.
- [ ] `mise run check` is green.

## Out of Scope

- Changing the contract's "liveness hint" wording.

## Comments

**Decision (2026-10-14): option 3 - group-based liveness, no new dependency.**
Before signalling, the recorded pid must be a live process-group *leader*, and
`reconcile_worker` uses that same group-based probe instead of a plain
process-liveness check.

Rationale: the analysis shows both kill paths already signal only a process
*group*, so a recycled pid that does not lead a group yields `ESRCH` and kills
nothing; the residual hazard is a recycled pid that also happens to lead a group,
and this closes it. Option 1 (`sysinfo`, or a crate-wide `unsafe` exception) is
avoided; option 2 would leave the hazard documented but unfixed.

Coupling: option 3 is only sound once **every** Run owns its process group.
Background Runs already do; foreground Runs do not, deliberately, because they
need the terminal's foreground group. That is exactly what 20/C4's decision
delivers, so the two are implemented and landed together.
