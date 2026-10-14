# Assert that a Run's session landed in its Worker Workspace

Status: ready-for-human
Type: task
Blocked by: 37

## Problem

jjfx relies on the spawned agent process's cwd to place a Run in its Worker
Workspace, and opencode honours it today. But it did not always: the Worker-cwd
report (ticket 05) was a real incident where opencode's session directory ended up
as the main repo, so a Worker ran its shell tools - and wrote its changes - in the
wrong worktree. Ticket 05 closed as an upstream opencode defect: no jjfx fix was
available, and **jjfx had no way to notice**.

opencode's server API exposes the session's directory, so jjfx *can* notice.

## What is available

`GET /api/session` returns, per session:

```
{"id": "ses_…", "location": {"directory": "/Users/jarvis/Code/…"}, …}
```

`location.directory` is the session's working directory. Verified live: a session
started in `jjfx-workspaces/ticket-07-20` reports exactly that path, and the
report's affected sessions would have reported the main repo instead. The
identification this needs is the same one ticket 37 establishes (match
`pool:worker-<id>:<ticket>` among active sessions), which is why it is blocked by
37 rather than duplicating it.

## The question

What should jjfx do when a Run's session directory is **not** its Worker Workspace?

1. **Warn only** - log it loudly and carry on. Cheapest, and no risk of blocking
   work on an experimental API or an ambiguous `location`, but the Worker still runs
   in the wrong place.
2. **Fail the Run** - mark it failed with a clear reason, so a misdirected Worker
   never silently commits to the wrong worktree. Requires trusting `location`.
3. **Refuse to dispatch** - check before the work starts and stop the Run early.

The API describes itself as experimental (spec `0.0.1`), so the check must be
**best-effort**: if the API or `location` is unavailable, jjfx must never fail a Run
over it. Whatever is chosen, the mismatch has to be visible - that is the whole
point of the ticket.

## Acceptance Criteria

- [ ] The chosen action is recorded in `## Comments`.
- [ ] A Run whose session directory is not its Worker Workspace is detected, with a
      test covering the mismatch path.
- [ ] An unreachable or older opencode API does not fail a Run.

## Out of Scope

- Fixing opencode's directory handling (ticket 05 - upstream).
- Stopping a Run (ticket 37).
