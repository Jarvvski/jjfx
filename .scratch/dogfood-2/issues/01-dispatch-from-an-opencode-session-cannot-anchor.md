# Dispatching from inside an opencode session cannot launch a Worker

Status: ready-for-agent
Type: bug
Blocked by: none

## Problem

`harness/08` creates a Worker's opencode session as a **child** of the session
that issued the Dispatch (`OPENCODE_SESSION_ID`). `create_run_session`
(`src/engine/opencode_api.rs:672`) then reads the session back and asserts
opencode anchored it to the Worker Workspace, failing the Run when it did not.

Against a real opencode, a child session inherits the **parent's** directory, so
the anchor opencode stores is the dispatching session's directory - not the
Worker Workspace jjfx asked for. jjfx fails closed (correct: it refuses to run an
agent in the wrong worktree) but **every Dispatch issued from an agent's own
shell fails at launch**, which is the documented primary use case for an agent
driving jjfx.

Observed from an opencode session whose cwd was the repository root:

    $ jjfx dispatch --all --json
    [
      {"ticket":"sift--01-bucket-counts","outcome":"failed","worker":"worker-1be12c",
       "phase":"launch","pid":null,
       "detail":"cannot anchor Worker worker-1be12c's opencode session: opencode anchored
       the Run's session to /Users/jarvis/Code/personal/jjfx-drive-2 instead of the Worker
       Workspace /Users/jarvis/Code/personal/jjfx-drive-2-workspaces/worker-1be12c"},
      {"ticket":"sift--02-grep-filter","outcome":"failed","worker":"worker-4af033",
       "phase":"launch","pid":null, "detail":"cannot anchor ... worker-4af033 ..."},
      ... 4 more held back by the cap ...
    ]

The identical Dispatch with the dispatching session removed succeeds:

    $ env -u OPENCODE_SESSION_ID jjfx dispatch --json sift--01-bucket-counts
    [{"ticket":"sift--01-bucket-counts","outcome":"dispatched","worker":"worker-1be12c",
      "detail":"session ses_ea66950a0ffe..."}]

So the failure is caused by `with_parent`, and the workaround an agent must
discover on its own is "unset `OPENCODE_SESSION_ID` before dispatching" - which
silently gives up the child-session topology `harness/08` was built for.

**Secondary - the failed launch leaks the session.** `jjfx children` lists both
orphaned child sessions (state `absent`, `directory` = the repo root, the
Workspace the Run never ran in), and nothing deletes them:

    $ jjfx children --json
    {"children":[{"directory":"/Users/jarvis/Code/personal/jjfx-drive-2",
      "session":"ses_ea669a284ffe...","state":"absent",
      "title":"pool:worker-1be12c:sift--01-bucket-counts"}, ...],
     "parent":"ses_ea66bbf98ffe...","source":"opencode"}

## Acceptance Criteria

- [ ] A Dispatch issued from inside an opencode session launches a Worker whose
      session is anchored to the Worker Workspace (or the child-session anchor
      assertion is reconciled with opencode's directory inheritance), so the
      agent-driven path dispatches.
- [ ] A launch that fails the anchor assertion removes the session it created,
      rather than leaving it for `children` to report as an orphan.

## References

- `src/engine/opencode_api.rs:672` (`create_run_session`), `:42`
  (`dispatching_session`), `:250` (`with_parent`)
- `.scratch/harness/issues/08-child-session-topology.md`
- `.scratch/dogfood-2/report.md` (the drive this was found in)

## Triage (2026-10-20)

Triaged from `needs-triage` to `ready-for-agent`. The evidence is a **live
reproduction during the second drive**, not a code reading: the drive hit this
while working a real backlog through jjfx, and the report
(`.scratch/dogfood-2/report.md`) records what happened around it.

Independently checked: this agent's own shell carries `OPENCODE_SESSION_ID`, and
`opencode_api` asserts the anchor opencode actually stored. The failing launch
itself was **not** re-reproduced here, because doing so spends a Run.

The acceptance criteria are unchanged - they were already specific and
testable, which is why this needed triage rather than a rewrite.
