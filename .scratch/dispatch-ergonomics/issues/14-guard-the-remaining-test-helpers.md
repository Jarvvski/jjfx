# Guard the remaining raw test helpers

Type: task
Status: ready-for-agent
Blocked by: none

## Question

*Stop the test suite leaking agent processes* fixed the demonstrated leaks: a raw `std::process::Child` in the two foreground tests let a helper blocked on a never-exiting Agent Runtime outlive the test, and the helper plus the stub's process group both stayed resident — 27 leaked groups, some alive for around twenty hours. `tests/support/mod.rs` now has `HelperGuard`, which reaps the helper and its recorded process group(s) on every exit path, including unwinding.

It did not convert every remaining raw `.spawn()` in the suite. The ones left alone — the PTY test, `reset_run_does_not_signal_a_group_whose_recorded_leader_is_gone`, and the worker-actions lock tests — were judged to strand at most a short-lived `sh` or helper, never a persistent agent, so what they can leak is litter rather than a twenty-hour resident group.

Decide whether that judgement holds and, if it does not, apply `HelperGuard` to them. The guard now exists and is generalised, so the change is small; the real question is whether the remaining spawns can be reasoned about as firmly as the ones already fixed, or whether "it exits promptly" is an assumption that a later edit can quietly break — which is exactly how the twenty-hour leak arrived.

## Answer

<!-- recorded on resolution -->
