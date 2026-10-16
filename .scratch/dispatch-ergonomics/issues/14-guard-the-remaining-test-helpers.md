# Guard the remaining raw test helpers

Type: task
Status: resolved
Blocked by: none

## Question

*Stop the test suite leaking agent processes* fixed the demonstrated leaks: a raw `std::process::Child` in the two foreground tests let a helper blocked on a never-exiting Agent Runtime outlive the test, and the helper plus the stub's process group both stayed resident — 27 leaked groups, some alive for around twenty hours. `tests/support/mod.rs` now has `HelperGuard`, which reaps the helper and its recorded process group(s) on every exit path, including unwinding.

It did not convert every remaining raw `.spawn()` in the suite. The ones left alone — the PTY test, `reset_run_does_not_signal_a_group_whose_recorded_leader_is_gone`, and the worker-actions lock tests — were judged to strand at most a short-lived `sh` or helper, never a persistent agent, so what they can leak is litter rather than a twenty-hour resident group.

Decide whether that judgement holds and, if it does not, apply `HelperGuard` to them. The guard now exists and is generalised, so the change is small; the real question is whether the remaining spawns can be reasoned about as firmly as the ones already fixed, or whether "it exits promptly" is an assumption that a later edit can quietly break — which is exactly how the twenty-hour leak arrived.

## Answer

The judgement holds for the worker-actions lock tests and fails for the other
two. Two of the three remaining sites can strand a persistent process on a
panic; both now use `HelperGuard`. Each was checked against the actual code, not
the "it exits promptly" shorthand.

**The PTY test, `foreground_run_reads_the_terminal_and_returns_it` - changed.**
`PtyOutput::drain` reads the PTY master on a background thread holding a clone
of the master fd, so a panic before the Run ends means the terminal never sees
EOF: the Agent's `read line` blocks forever, the helper stays blocked on it in
`run_foreground`, and the stub's process group stays resident. That is a
persistent `claude` stub plus its helper - the ticket 10 class, not litter.
Reproduced with a raw `Child`: a panic after `agent-ready` left the helper, the
`jjfx __foreground` wrapper, and the `claude` stub alive; the same panic with
`HelperGuard` leaves nothing. The stub now records its process group
(`ps -o pgid= -p $$`) before it blocks, and the test hands that file to
`HelperGuard::spawn`, so the guard kills the group the wrapper leads.

**`reset_run_does_not_signal_a_group_whose_recorded_leader_is_gone` - changed.**
The descendant is deliberately *not* short-lived: it is a `sh` that ignores TERM
and loops forever, and the test's own cleanup runs only after several
assertions, so an earlier panic strands it. Reproduced: a panic before the
cleanup left the group alive with the raw spawn; the guard reaps it.
`HelperGuard` gains `reap()`, which reaps a helper that exits on its own while
keeping the guard armed for the group it left behind (the leader records its own
PID, and the descendant is the only member of that group).

**The worker-actions lock tests - left alone.** In
`send_waits_for_the_workspace_operation_lock` and
`rebase_waits_for_the_workspace_operation_lock` the helper's only blocker is an
`flock` the test itself holds. The `lock_file` fd closes on every exit path,
including unwinding, so the helper always unblocks; once it does, its work
spawns either a fixture that exits at once (an `exit 0` claude stub) or no agent
at all (Rebase runs only `jj`/`gh` stubs). Nothing keeps the helper blocked, so
the judgement holds here and the raw spawns stay.

The rest of the suite's raw `.spawn()` calls - `logs.rs`, `wait.rs`,
`adhoc_ticket.rs`, `orchestration.rs`'s `live_group_leader`,
`spawn_group_leader` / `spawn_non_group_leader`, and `worker_pool`'s destroy
helper - spawn the `jjfx` CLI or a bounded `sleep`, never a helper running the
Agent Runtime, so ticket 10's list was complete for the agent-spawning sites.

Verified: `mise run check` green, and the suite leaves no `claude` / `codex` /
`opencode` stub process behind.
