# `jjfx logs` follows a finished Worker forever

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

`jjfx logs <worker>` prints the log tail and then follows the Run - but the follow loop has no terminal check, so it never returns.

```rust
print_log_tail(logs.path())?;
if worker_is_terminal(repository, &worker) {
    return Ok(());                       // checked once, before following
}
eprintln!("Following {} log for {worker}", logs.runtime());
let mut last = None;
loop {
    if let Some(activity) = logs.open().current_activity()? {
        ...
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
}
```

(`src/cli.rs:2101-2110`.) `worker_is_terminal` guards the *start* only: a Worker that is already finished returns cleanly, but a Worker that is running when you attach keeps the command alive after its Run ends.

The comment two lines above explains the original worry - "a terminal Worker whose log does not parse would otherwise follow forever with no output" - so the fix stopped the silent case without ever giving the loop an exit.

Two consequences: a human has to Ctrl-C a command whose work is already done, and an agent that reaches for `logs` as a way to wait gets a hang rather than a return. (The `wait` ticket adds the blocking command agents actually want; this one makes `logs` behave.)

## Solution

Give the follow loop an exit. Re-check terminal state on each iteration - the loop already sleeps 200ms, so it is one more cheap check - and return cleanly when the Worker goes terminal. Keep printing the tail first, as it does now, so nothing is lost.

If the log disappears or the Worker's Run is reconciled away mid-follow, exit cleanly rather than erroring, unless the failure is genuinely worth reporting.

## Acceptance Criteria

- [ ] `jjfx logs <worker>` on a running Worker returns by itself when the Run reaches a terminal state.
- [ ] Attaching to an already-terminal Worker still returns immediately, as it does today.
- [ ] The command does not spin after the Run ends.
- [ ] A test covers the follow-until-terminal path.
- [ ] `mise run check` is green.

## References

- `src/cli.rs:2086-2111` (`logs_command`, `LOG_TAIL_BYTES`, `print_log_tail`)
- `src/cli.rs:2139` (`worker_is_terminal`)
- `.scratch/dispatch-ergonomics/issues/11-wait-for-a-worker.md` (the blocking command, for contrast)

## Comments

- 2026-10-16: Found while designing `jjfx wait` - the only existing blocking surface turns out to be a hang.
