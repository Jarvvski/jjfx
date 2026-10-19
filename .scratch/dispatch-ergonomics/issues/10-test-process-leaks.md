# Stop the test suite leaking agent processes

Status: resolved
Type: task
Blocked by: none

## Problem

The test suite leaves `claude` processes running. After a day of `mise run check`, `ps` showed a dozen orphans:

```
jarvis  20409 ... /bin/bash /var/folders/.../bin/claude -p --output-format stream-json --verbose \
  --settings {...} --append-system-prompt ... reserved foreground test
jarvis  68884 ... ... reset foreground test
...
```

Alive since the previous day, one to six minutes of CPU each, still resident. They are the stub runtimes the tests spawn, and nothing is reaping them.

This is not only untidy: a leaked process holding a Workspace or a lock is exactly the shape of an intermittent failure, and this repository has already had to make its process and exec timing tests deterministic. It may also be a symptom of a child that is spawned outside the test's control, which is the same class of bug in the product.

## Solution

Reproduce it first, then find where a test spawns a child that outlives the test - a process group that is never killed, a child that detaches, or a handle dropped without a wait - and make the test own its child's lifetime. A test that spawns a process should kill and reap it on every exit path, including panic; a helper or a drop guard is the usual answer.

If the reproduction shows the leak is in product code rather than test code, fix it there and say so.

## Acceptance Criteria

- [x] Reproduce it first: run the suite and show the leaked processes - or show they are already gone and identify the mechanism.
- [x] The suite leaves no `claude` / `codex` / `opencode` process behind when it finishes.
- [x] The fix covers the failure path: a panicking test must not leak.
- [x] `mise run check` is green.

## References

- `tests/` (process and exec timing tests)
- `tests/agent_runtime.rs`
- `.scratch/bug-hunt/issues/42-flaky-process-and-exec-tests.md` (the earlier determinism work)

## Comments

- 2026-10-16: Found while setting up the `todo-md` test-drive: a dozen orphaned stubs, `reserved foreground test` / `reset foreground test`.
- 2026-10-16: Resolved. Reproduced first: 27 leaked stub groups (54 `/bin/bash
  .../bin/claude` processes, in leader/descendant pairs) plus 27 orphaned
  `agent_runtime` helpers, all reparented to PID 1 and alive since earlier runs.
  Root cause: `cross_process_reset_terminates_a_live_foreground_run` and
  `failed_foreground_launch_leaves_no_surviving_descendants` spawned their
  helper with a raw `Child`. The helper runs the real supervisor, which spawns
  the stub runtime into its own process group, and both the helper and the stub
  ignore TERM. A `Child` is not killed when it drops, so any panic before the
  test's own cleanup left the helper blocked on a runtime that never exits, and
  both stayed resident forever - the product was not at fault.
  Fix: `tests/support/mod.rs` gains `HelperGuard`, generalised from the local
  `BackgroundHelperGuard`, which owns the helper and the recorded process
  group(s) and reaps both on every exit path, including unwinding. The two
  foreground tests and `worker_pool`'s live-process destroy test use it.
  Regression test: `a_panicking_test_reaps_its_helper_and_the_agent_process_group`
  panics inside `catch_unwind` and asserts the group is gone; it fails and
  leaks without the guard, and passes with it. `mise run check` green, and the
  full suite leaves no `claude` / `codex` / `opencode` process behind.

## Resolution

Reconciled Mon 19 Oct 2026 against the merged code at `6d6f3b689e0e`.

This ticket was marked resolved without its acceptance boxes ticked. Every criterion was re-verified by an independent read-only review of the code and tests on `main`, and `mise run check` was re-run green on `6d6f3b689e0e`. The reconciliation recorded no new work; it confirms what shipped.

Caveat: several tests still spawn helpers with a raw `Child`. The guard closes the two identified leak sites, and a full `mise run check` on this tree left no helper, no `claude`/`codex`/`opencode` stub and no test binary behind.
