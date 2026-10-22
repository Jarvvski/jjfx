# Concurrent jjfx operations can diverge the op log and swallow a commit

Status: resolved
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

- [x] Reproduce it, or rule it out with evidence: do concurrent jjfx operations
      across Workspaces diverge the op log, and under what concurrency?
      (`35` and `42` both left load harnesses; `43` reconstructed one at
      `.scratch/bug-hunt/load-harness.sh`.)
- [x] If it is reproducible: jjfx either serialises the operations that can
      diverge, or **detects and reports** the divergence - it must not be
      possible for a worker to lose committed work without being told.
- [ ] If it is not reproducible: record what happened in this instance, name
      what was ruled out, and close it - as `43` was.
- [x] Whatever the outcome: say whether a worker can be told "your commit is no
      longer reachable", and if it cannot, file that rather than leaving it in
      this ticket's prose.

## References

- `.scratch/bug-hunt/issues/43-concurrent-provisioning-blocks-under-load.md`
- `docs/adr/0009-workspace-dispatch-domain.md` (the concurrency model)
- Sighted while implementing `.scratch/dogfood/issues/13-*`

## Resolution (Wed 21 Oct 2026)

**Reproduced, and fixed.** The third box stays unticked because it is the "not reproducible" outcome and this is the other one.

The divergence is real, it is jj's normal lock-free behaviour, and **jjfx caused it**: the rollback jjfx ran after a failed push was `jj undo --allow-cross-workspace`. A `jj undo` restores the recorded working-copy commit of *every* Workspace, so undoing a sibling's operation rewrites that sibling's working copy underneath it. Racing the sibling's own `jj commit`, jj reconciles the two rewrites into a **divergent** change - two commits sharing one change id - and the sibling's work appears swallowed, with nothing reported. That is exactly the `dogfood/13` shape.

### The reproduction

`.scratch/bug-hunt/op-log-harness.sh` (new) drives the jj invocations jjfx issues concurrently across N Workspaces and reports, per round, whether a change id ended up divergent (two commits, one change id) or whether a marker a Workspace committed stopped being reachable. It has three modes: `mixed` (the jjfx operation set - new+commit, weld, absorb, reset, tidy, lift_all), and the two isolating ones, `undo-cross` (`jj undo --allow-cross-workspace`, what jjfx did) and `undo-plain` (a plain `jj undo`, the fix).

`undo-cross`, 3 runs x 20 rounds x 6 Workspaces, 15 repository-wide undos a round (hundreds of concurrent commits each):

```
divergent_total = 76, 92, 116   missing_markers_total = 0
```

`undo-plain`, same shape:

```
divergent_total = 0, 0, 0       missing_markers_total = 0
```

The divergent commits are a per-round `jj commit`: two commits, one change id, one carrying the Worker's file and one empty, both with the same parent - the Worker's change "swallowed into a divergent head", exactly as reported. The `keep-` marker a Workspace committed at setup stayed reachable in every run, so the harness distinguishes "diverged" from "gone".

### What was ruled out

Each of these was hammered against concurrent Worker commits and produced **no** divergence, so none of them is the cause:

- a single `jj undo --allow-cross-workspace` per round (80 rounds);
- `jj rebase -s roots(mutable() & mine() & ::working_copies()) -d trunk()` (`lift_all`, 80 rounds) - the TUI's repository-wide lift;
- `jj abandon` of the tidy revset;
- `jj op restore <op>` - the guarded `jjfx undo` primitive;
- the same `tidy` + `lift_all` together (25 rounds).

The op-log heads themselves diverge routinely and transiently under any concurrency - that is jj's documented lock-free design, and jj reconciles them (`reconcile divergent operations`). That is *not* a defect and is not what this ticket is about; the defect is that the flag turned a transient op-head divergence into a permanent **commit** divergence.

### The fix

`src/engine/worker_actions.rs`: the rollback is now a plain `jj undo`. jj refuses to undo an operation performed in another Workspace, so a rollback can only undo an operation this Workspace made; when a concurrent sibling operation is the last one, jj refuses and the refusal is returned as a failed rollback rather than silently rewriting the sibling. The three rollback call sites (rebase push failure, absorb push failure, absorb-rewrote-the-mainline) all go through the one `Commands::undo`, so one change covers them. A `momo fix` entry was added.

New coverage: a hermetic unit test (`engine::worker_actions::tests::undo_does_not_cross_into_another_workspace`) builds a two-Workspace throwaway repository, commits in one, and asserts a rollback from the other **refuses** and leaves the sibling's commit untouched (it fails if the flag is put back). Proved both ways - with the developer's jj config and with `JJ_CONFIG` pointed at an empty file - both pass.

### Can a Worker be told "your commit is no longer reachable"?

**Not today, and it is filed.** jjfx reads the operation log but never reads jj's `divergent()` revset or otherwise checks whether a Workspace's commit is still reachable, so a Worker whose commit diverged or went hidden is told nothing. This ticket removes jjfx's own path to causing it, but the capability to *report* it is absent. Filed as [46](46-a-worker-is-never-told-its-commit-diverged.md).

### Gate

`mise run check` - green (fmt + clippy + build + tests). See the landed commit.
