# Dispatching from inside an opencode session cannot launch a Worker

Status: resolved
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

- [x] A Dispatch issued from inside an opencode session launches a Worker whose
      session is anchored to the Worker Workspace (or the child-session anchor
      assertion is reconciled with opencode's directory inheritance), so the
      agent-driven path dispatches.
- [x] A launch that fails the anchor assertion removes the session it created,
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

## Resolution

The root cause is opencode's contract, not a bug in jjfx's assertion: **a session
created with a `parentID` is anchored to its parent's location**, and opencode
offers no way to move it there (`session.create` ignores `location` when
`parentID` is set, and `session.move` leaves a child where its parent is).
Verified live against opencode v2.1.1: a child requested at `/tmp` under a parent
in a jjfx workspace was stored at the parent's directory, and `session.move` on
the child was a no-op while the same call moved a root session.

A Run must execute in its Worker Workspace - that anchor is the guard that keeps
an agent out of the wrong worktree - so the anchor wins over the child-session
topology. `create_run_session` now asks whether the dispatching session is itself
anchored to the Worker Workspace (`nestable_parent`): only then does it send
`parentID`. Otherwise the Run's session is created as a **root** session anchored
to the Worker Workspace, which opencode honours, so a Dispatch issued from an
agent's own shell launches a Worker with no workaround. A parent jjfx cannot read
is not honoured either - the Run gets a session jjfx can vouch for.

A launch that fails after `session.create` now removes the session it created
(`OpenCodeApi::delete_session`, the `session.remove` route) before returning the
error, so `children` never reports an orphan nothing drives. The removal is
best-effort: a failed removal is appended to the anchor reason rather than
replacing it.

Verified: `mise run check` (changelog validation + fmt + lint + build + test)
passes. Unit tests over the `OpenCodeRunner` seam cover the child kept when the
dispatcher is in the Workspace, the root session when it is elsewhere or
unreadable, the removal on a mismatched anchor and on a rejected read-back, a
failed removal that does not hide the anchor failure, and `session.remove`'s own
success and rejection. `tests/agent_runtime.rs` drives the real reserved Run
against a fake `opencode` that emulates the inheritance rule:
`opencode_run_anchors_a_root_session_when_the_dispatching_session_is_elsewhere`
proves the agent-driven path starts, and
`opencode_run_fails_when_the_session_is_anchored_elsewhere` now also asserts the
session was removed. `tests/dispatch_outcomes.rs` drives the built binary:
`a_dispatch_from_an_opencode_session_launches_a_worker` runs
`OPENCODE_SESSION_ID=ses_dispatcher jjfx dispatch --all --json` against a fake
`opencode` whose dispatching session is anchored to the repository, and asserts
`outcome: dispatched` with a root session anchored to the Worker Workspace - the
exact reproduction in this ticket, red before the fix (`cannot anchor Worker ...
instead of the Worker Workspace`) and green after.

Docs updated: `docs/lifecycle-integration.md` (parentage is conditional on the
dispatcher's anchor), `skills/jjfx-dispatch/PLAYBOOK.md` (§3a is now a fixed trap,
and the checklist no longer teaches `env -u OPENCODE_SESSION_ID`), and the
unreleased changelog entry for the child-session topology.

**Not done, deliberately:** the child-session topology (`harness/08`) is now
unreachable in the common case, because a dispatching agent runs in the
repository rather than the Worker Workspace. Recovering it needs opencode to
anchor a child elsewhere, or jjfx to record the dispatcher link in its own state
rather than opencode's `parentID`; both are a design change beyond this bug.
