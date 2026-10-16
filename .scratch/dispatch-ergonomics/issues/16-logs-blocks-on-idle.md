# `jjfx logs` still blocks on a stale idle Worker

Type: task
Status: resolved
Blocked by: none

## Question

*`jjfx logs` follows a finished Worker forever* stopped follow mode when a Run reached a terminal state, and when a Worker left the Pool mid-follow. It did **not** cover the `Idle` state, and said so rather than widening scope silently: a Worker reset to `idle` keeps its `log_file` — nothing clears it — so `jjfx logs <idle-worker>` still blocks forever on a log that will never grow again.

That is the same hang, one state over. And `idle` is the state a Worker sits in *between* Runs, so it is plausibly the case a caller hits most often.

The reason it was left alone is a real trade-off rather than an oversight: `idle` is not necessarily final. A Worker can be re-dispatched and the same `log_file` is reused, so stopping at `idle` would make `logs` return just before an imminent busy transition. Decide which is right:

- **Follow the current Run, not the Worker** — an idle Worker has no Run, so `logs` returns; a caller that wants the next Run re-attaches, and `jjfx wait` is the purpose-built way to wait for one. This makes the rule a single sentence and removes the hang.
- **Keep blocking through `idle`**, and solve the hang another way — a timeout, or an explicit flag. More faithful to "follow this Worker's output", at the cost of a mode that can block indefinitely with nothing to say.

The first is recommended. `logs` should answer "what did this Run print", and the ticket that filed this one already established that a finished thing must not hold a caller forever.

## Answer

**Follow the current Run, not the Worker** - the recommended option. `logs` now stops following on `idle` as well as on the terminal states ticket 12 covered, so a Worker with no Run ends the follow instead of holding the caller on a log nothing will append to.

The reachable hang is a Worker reset *during* a follow - a human runs `logs`, then `jjfx pool reset`. The command has already captured the log path, and a Reset leaves the file on disk (it only clears the state's `log_file` field), so the loop kept re-reading a file that could never grow again and never returned. `follow_stop` now returns `FollowStop::Idle` for `WorkerStatus::Idle`, so the loop reports `Stopped following <worker>: the Worker is idle` and returns `0`, the same shape as ticket 12's other stops.

The ticket's literal case - `logs <idle-worker>` blocking forever - needs a stale `log_file` on the idle state to bite, and a Reset clears that field (`clear_run_fields`), so `logs` on a reset Worker already failed fast with `no Run log` rather than hanging. The one rule still covers the case where an idle state does point at a log file: `logs` now returns at once with just the tail, as attaching to an already-terminal Worker does.

The trade-off ticket 12 declined is real but small: a Worker can be re-dispatched and reuse its log file, so stopping at `idle` can return just before an imminent `busy`. That is the point of the decision - `logs` answers "what did this Run print", and a caller that wants the next Run re-attaches or, better, runs `jjfx wait`, which is the purpose-built way to block for one.

Tests: `tests/logs.rs` gains `logs_returns_immediately_for_an_idle_worker` and `logs_returns_when_a_running_worker_is_reset_to_idle`. Both hang on the pre-fix code and return on the fix. `mise run check` is green.

## Comments

- 2026-10-16: Resolved. `logs` treats `idle` as a stop, so the follow loop has an exit for the state a Worker sits in between Runs; the changelog records it as a fix.
