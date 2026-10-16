# Stop the test suite leaking agent processes

Status: ready-for-agent
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

- [ ] Reproduce it first: run the suite and show the leaked processes - or show they are already gone and identify the mechanism.
- [ ] The suite leaves no `claude` / `codex` / `opencode` process behind when it finishes.
- [ ] The fix covers the failure path: a panicking test must not leak.
- [ ] `mise run check` is green.

## References

- `tests/` (process and exec timing tests)
- `tests/agent_runtime.rs`
- `.scratch/bug-hunt/issues/42-flaky-process-and-exec-tests.md` (the earlier determinism work)

## Comments

- 2026-10-16: Found while setting up the `todo-md` test-drive: a dozen orphaned stubs, `reserved foreground test` / `reset foreground test`.
