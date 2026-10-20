# Concurrent jjfx operations can diverge the op log and swallow a commit

Status: ready-for-agent
Type: bug
Blocked by: none

## Problem

While `dogfood/13` was being implemented, a sibling worker's concurrent `jj`
operations made jj report `reconcile divergent operations`, and **swallowed one
of the worker's commits into a divergent head**. The worker noticed, abandoned
the divergence and re-applied the edit by hand, so nothing was lost that time.

That is the shape worth caring about: **a committed change disappeared, and the
only thing that caught it was the worker reading its own history carefully.** A
worker that did not look would have continued from a tree missing its work.

This is not obviously jj's fault alone, and the ticket is not to blame jj. jjfx
is *built* to run many concurrent jj operations across Workspaces that share one
repository and therefore one operation log, and it is jjfx's job not to let a
worker lose committed work to that - silently or otherwise. It connects to a
caveat this batch already recorded: jj attributes an operation to **no tool and
no process**, so after the fact there is no way to say which agent's operation
diverged the log.

## Acceptance Criteria

- [ ] Reproduce it, or rule it out with evidence: do concurrent jjfx operations
      across Workspaces diverge the op log, and under what concurrency?
      (`35` and `42` both left load harnesses; `43` reconstructed one at
      `.scratch/bug-hunt/load-harness.sh`.)
- [ ] If it is reproducible: jjfx either serialises the operations that can
      diverge, or **detects and reports** the divergence - it must not be
      possible for a worker to lose committed work without being told.
- [ ] If it is not reproducible: record what happened in this instance, name
      what was ruled out, and close it - as `43` was.
- [ ] Whatever the outcome: say whether a worker can be told "your commit is no
      longer reachable", and if it cannot, file that rather than leaving it in
      this ticket's prose.

## References

- `.scratch/bug-hunt/issues/43-concurrent-provisioning-blocks-under-load.md`
- `docs/adr/0009-workspace-dispatch-domain.md` (the concurrency model)
- Sighted while implementing `.scratch/dogfood/issues/13-*`
