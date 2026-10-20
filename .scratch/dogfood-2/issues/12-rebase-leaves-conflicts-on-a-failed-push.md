# `rebase` mutates the Workspace and then fails, leaving conflicts behind

Status: needs-triage
Type: bug
Blocked by: none

## Problem

`WorkerActions::rebase` (`src/engine/worker_actions.rs:466`) rebases the Worker's
bookmark onto `main` and *then* pushes; if the push fails it reports the failure
but leaves the rebase in place. When `main` has moved under the Worker - the
normal case once sibling PRs have merged - the rebase produces conflicts, and the
Worker is left in a conflicted state the operator did not ask for:

    $ jjfx rebase worker-ccb8a5
    Error: cannot push rebased branch adam/sift--08-sparkline: Error: Won't push
    commit 5423cedb8924 since it has conflicts
    Hint: Rejected commit: uvksku 5423ce adam/sift--08-sparkline* | (conflict)
    sift: render bucket counts as a sparkline (--format spark); rollback: None

(`rollback: None` - nothing was undone.) The Workspace now holds a conflicted
bookmark; `jjfx status` is unaffected but the next `open-pr` / `review` sees a
branch that is not what the Worker delivered.

This is the shape `dogfood/05` was filed for ("landing verbs must be atomic:
resolve the precondition before mutating"). The bookmark *is* resolved first here;
the remaining non-atomicity is rebase-then-push.

## Acceptance Criteria

- [ ] A `rebase` that cannot push leaves the Workspace as it found it (undo the
      rebase), or the failure is reported as "rebased, conflicts to resolve" with
      the conflicted state as a deliberate, named outcome.
- [ ] The push's conflict refusal is not surfaced as a generic command failure
      with a `rollback: None` that implies a rollback happened.

## References

- `src/engine/worker_actions.rs:466` (`rebase`)
- `.scratch/dogfood/issues/05-*.md` (atomicity), `.scratch/dogfood/issues/02-*.md`
