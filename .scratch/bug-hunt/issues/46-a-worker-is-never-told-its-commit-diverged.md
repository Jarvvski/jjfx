# A Worker is never told its commit has diverged or is no longer reachable

Status: needs-triage
Type: task
Blocked by: none

## Problem

jj's operation log is deliberately lock-free: concurrent `jj` processes each see a consistent view, write their own operation, and jj reconciles divergent operations on the next command. A concurrent operation can therefore leave a Worker's commit in one of two states jj calls a *divergence*:

- **divergent** - two commits share one change id (e.g. a repository-wide `jj undo` restored a Workspace's working-copy commit while that Workspace's own `jj commit` rewrote it), or
- **hidden** - the commit was rewritten or restored out of the visible set, so `jj log` no longer shows it even though the object survives in the op log.

jjfx reads the operation log (`src/jj.rs::operation_log` / `operation_id`) and drives `jj op restore` for undo (`src/engine/undo.rs`), but **nothing in jjfx reads jj's `divergent()` revset or otherwise checks whether a Workspace's commit is still reachable**. A Worker whose commit diverged or went hidden is never told; the only thing that catches it is the Worker reading its own history carefully.

That is exactly what happened in the `dogfood/13` sighting behind ticket 44: the Worker noticed a divergent head by hand and re-applied its edit, so nothing was lost *that time*. Ticket 44 removed jjfx's own path to causing that divergence (a failed-push rollback no longer passes `jj undo --allow-cross-workspace`), but it did not add the capability to *tell* a Worker - and a divergence can still be caused by anything else running against the shared log (another agent's `jj`, a repository-wide `jj op restore`, an owner's `jj` in another Workspace).

## Why this is worth a ticket rather than prose

The batch already recorded that **jj attributes an operation to no tool and no process**, so after the fact there is no way to say which agent's operation diverged the log. The one thing jjfx *can* do is read the result: jj exposes divergence as structure (the `divergent()` revset, the `??` marker in `jj log`), and jjfx owns the Workspace model, so it can say "Workspace `worker-...`'s commit is now divergent" rather than leaving the Worker to discover it.

## Acceptance Criteria

- [ ] jjfx reads whether a Workspace's commit has diverged or become unreachable from the visible set, from a machine-readable jj source (jj's `divergent()` revset, not jj's English prose), and surfaces it - a `status`/`undo` field or a verb - so a Worker is told rather than having to read its own history.
- [ ] The surface distinguishes "this commit diverged" from "this commit is gone" (hidden or abandoned), or says plainly which it cannot tell apart.
- [ ] A test drives the state (a commit made divergent by a concurrent operation in a throwaway repository) and asserts jjfx reports it; hermetic per `AGENTS.md`'s "Test hermeticity (CI parity)".
- [ ] If it cannot be done from a stable jj source, say so and close with what was ruled out.

## References

- `.scratch/bug-hunt/issues/44-concurrent-operations-diverge-the-op-log.md` (the reproduction and the rollback fix)
- `.scratch/dogfood/issues/13-open-pr-docs-contradict-the-tool.md` (the sighting)
- `src/jj.rs` (`operation_log`, `operation_id`, `restore_operation`)
- `src/engine/undo.rs` (`UndoOutcome::Refused { later: None }` is the closest existing "cannot tell" surface, and it is about the undo record, not a commit)
