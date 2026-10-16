# `jjfx wait`: block until a Worker is done, with the answer in the exit code

Status: resolved
Type: task
Blocked by: 03

## Problem

An agent driving jjfx has no way to wait for a Worker to finish, so it polls with `sleep`:

```sh
jjfx dispatch --all --json
sleep 30
jjfx status --json | jq ...
```

That is wrong three ways. A fixed sleep can return *before* the Worker finishes - a false "done", which is the failure mode that matters. It overshoots, so every wait pays the worst case. And every poll costs an agent a turn.

What exists today does not cover it:

- **`dispatch --fg`** blocks, but only for the Dispatch you are issuing, for all of its Runs, inside that one call. It cannot re-attach to a Run started by a previous session, another agent, or a human.
- **`logs <worker>`** looks like a wait and is a hang. Its follow loop has no terminal check - `worker_is_terminal` is tested once *before* following, then `loop { ...; sleep(200ms) }` with no break (`src/cli.rs:2101-2110`) - so on a running Worker it never returns, even after the Run ends. See the sibling ticket.

## Solution

Add a blocking command whose exit code *is* the confirmation:

```sh
jjfx wait <worker>... [--until done|failed|terminal|ticket-resolved]
                      [--timeout <secs>] [--json] [--all]
```

**The exit code carries the answer.** "Not done" only exists if the call is allowed to give up, so the timeout is half the interface rather than a safety net:

- `0` - the condition was reached.
- `1` - timed out: the Worker is *not* done. This is the negative confirmation.
- `2` - the Worker reached a terminal state that does not satisfy `--until` (it failed while you waited for `done`).

If timeout and failure share a code, a caller cannot tell "still working" from "broke", and has to parse prose again.

**No lost wakeup.** A Worker already in the waited-for state must return immediately. This will happen: dispatch returns, a short Run finishes, and the wait arrives afterwards.

**Wait on the thing, not the Worker.** `--until ticket-resolved` is the real end-to-end confirmation. `--until terminal` (the default) is the honest one until ticket 03 makes resolution reliable.

**This is the reconciliation loop.** A Worker's state only advances when something reconciles it, so a `wait` that merely re-reads `.jj/pool/*.json` can hang on a Worker whose process died silently. `wait` should be "reconcile until the condition holds" - the same pass ticket 03 introduces, run repeatedly. Build them as one mechanism, not two commands that drift.

Precedent for the shape: `kubectl wait --for=condition=...`, `docker wait`, `docker compose up --wait`, `gh pr checks --watch`.

## Acceptance Criteria

- [ ] `jjfx wait <worker>` returns when the Worker reaches the default condition, and immediately if it is already there.
- [ ] `--timeout` bounds the wait and exits with a code distinct from both success and a failed Worker.
- [ ] A Worker that fails while waiting for `done` is distinguishable from a timeout.
- [ ] Multiple workers and `--all` are supported.
- [ ] `--json` prints the terminal Worker object(s) in the shape `status --json` already uses.
- [ ] `jjfx help wait` documents the conditions and the exit codes.
- [ ] `jjfx help dispatch` states how `--fg` relates to `wait`.
- [ ] Tests cover: already-terminal, timeout, failure, and the multi-worker case.
- [ ] `mise run check` is green.

## References

- `src/cli.rs:2101-2110` (`logs_command`'s follow loop), `:2139` (`worker_is_terminal`), `:1177` (`reconcile_runs`)
- `src/engine/foreground.rs` (how `dispatch --fg` waits)
- `.scratch/dispatch-ergonomics/issues/03-local-ticket-writeback-deadlock.md` (the reconcile work this builds on)

## Comments

- 2026-10-16: Filed at the owner's request, after the `todo-md` test-drive in which an agent had to poll with sleeps.
- 2026-10-16: Resolved. `jjfx wait <worker>... [--until done|failed|terminal|ticket-resolved] [--timeout SECS] [--json] [--all]` blocks until the condition holds and carries the answer in its exit code: `0` reached, `1` timed out, `2` a Worker reached a terminal state that does not satisfy `--until`. It is reconcile-until-a-condition, sharing the one reconcile pass (`reconcile_pass`: `reconcile_runs` then the local Ticket writeback) with `status` and `ticket reconcile`, so a Worker whose process died silently still advances and a finished Run still resolves its local Ticket. The condition is evaluated before the first sleep, so an already-terminal Worker returns at once. `--until ticket-resolved` needs `[tickets] source = "local"` and a Ticket on every target.
