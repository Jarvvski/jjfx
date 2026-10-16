# Dispatch must not report success when nothing happened

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

`jjfx dispatch` exits 0 when every Ticket failed, and reports `orchestrating` for a background orchestration that dies immediately. A scripting caller cannot tell success from failure - which matters most for the caller jjfx is built for, an agent driving the pool.

Every failed dispatch in a test-drive returned `EXIT: 0`:

```
$ jjfx dispatch --all --json
[ { "outcome": "failed", "phase": "workspace", ... }, { "outcome": "failed", "phase": "workspace", ... } ]
EXIT: 0
```

The same in the `identity` phase, and again for a capacity failure that "failed" while performing the only useful thing in the invocation. There is no exit-code path keyed on outcomes - `dispatch_command` returns `Ok(())` and `main` maps that to 0.

The background orchestration path is the same lie in a different shape:

```
$ jjfx dispatch todo-md --json
[ { "ticket": "todo-md", "outcome": "orchestrating", "worker": null, "detail": "orchestrating in background" } ]
$ ps aux | grep '[j]jfx-bin' | wc -l
       0
```

Nothing was spawned, nothing changed state, no log was written - and the foreground equivalent fails outright. The background path is swallowing a real error.

## Solution

- **Exit code:** exit non-zero when *every* Ticket failed. A batch with at least one `dispatched`, or one deliberately held back (`waiting`, from a capacity or spend cap), is still success.
- **Background orchestration:** do not print `orchestrating` and return before the child has proved it started. Either write the child's stderr to a log and put its path in the JSON, or perform the fetch and prepare synchronously in the parent so the error surfaces immediately. `"orchestrating in background"` must not be printable when the background immediately dies.

## Acceptance Criteria

- [ ] `jjfx dispatch` exits non-zero when every Ticket failed, and zero when at least one was dispatched or deliberately held back.
- [ ] A test covers the exit code for both cases.
- [ ] A background orchestration that fails is reported as a failure, not as `orchestrating`, with the child's error reachable (a log path in the JSON, or an immediate error).
- [ ] `mise run check` is green.

## References

- `src/cli.rs` (`dispatch_command`, `render_dispatch_result`, `render_dispatch_entries`)
- `src/main.rs` (exit-code mapping)
- `src/engine/worker_actions.rs` (`DirectDispatchOutcome`)

## Comments

- 2026-10-16: Filed from the `todo-md` test-drive. Two independent paths (`dispatch <parent>` and `dispatch <typo>`) showed the same silent-success shape, so treat it as a pattern rather than a one-off.
