# Set the Run's session directory explicitly

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

jjfx relies on the spawned agent process's cwd to place a Run in its Worker
Workspace. opencode honours it today, but it did not always: the Worker-cwd report
(ticket 05) was a real incident where a session's directory ended up as the main
repo, so a Worker ran its shell tools - and wrote its changes - in the wrong
worktree. Ticket 05 closed as an upstream opencode defect, which left jjfx depending
on an implementation detail of how opencode derives a session's directory from the
process cwd.

opencode's API lets jjfx **set** that directory instead of inferring it.

## What is available

Verified against opencode v2.0.29 (`opencode api GET /openapi.json`):

| Operation | Route | Body |
|---|---|---|
| `session.create` | `POST /api/session` | `{ location: { directory: <AbsolutePath> }, title, agent, model, … }` |
| `session.get` | `GET /api/session/{sessionID}` | - |
| `session.move` | `POST /api/session/{sessionID}/move` | `{ directory: <AbsolutePath> }` (required) |

`Location.PublicRef` is `{directory: <abs path>}`; `AbsolutePath` is a plain string.

## The approach - verified by experiment

Create the session **first**, with the Worker Workspace as its `location`, then run it
exactly as today with `opencode run --session <id>`.

Experiment (2026-10-14): a session was created via `session.create` with
`location.directory = <ws>`, then driven with `opencode run --session <id>` from a
*different* directory `<main>`. Its shell tool ran in **`<ws>`**:

```
process cwd:       /private/.../oc-loc-test/main
session location:  /private/.../oc-loc-test/ws
$ pwd
/private/.../oc-loc-test/ws
```

So the session's `location` wins over the process cwd, and the process cwd stops
mattering. That is the whole fix: jjfx tells opencode where to run instead of
inferring it.

## Scope

1. Create the Run's session via `session.create` with `location.directory` set to the
   Worker Workspace, and the `title` jjfx already uses
   (`pool:worker-<id>:<ticket>`).
2. Pass the returned id to the Run as `--session <id>`, as today.
3. **Best-effort assertion.** Once the session exists, confirm via
   `session.get`/`session.list` that `location.directory` is the expected one; if it
   differs, surface it (fail the Run with a clear reason rather than let it commit
   into the wrong worktree). If the API is unreachable, or an older opencode lacks
   these routes, fall back to today's behaviour and do **not** fail the Run.
4. Keep setting the process cwd as today - it is still correct, and it is the
   fallback when the API cannot be used.

## Interaction with ticket 37

This makes 37 **simpler**. Because jjfx now *creates* the session, it knows the
session id and does not need to match `session.list` by title. 37's identification
should be revisited once this lands, and its decision recorded accordingly. Note
that 37's rejection of "mint the id up front" does not apply here: this is not
minting an id in opencode's namespace, it is creating a session and keeping the id
opencode returns.

## Acceptance Criteria

- [ ] A Run's session is created with its Worker Workspace as `location`.
- [ ] A Run driven from the wrong process cwd still executes in its Worker Workspace.
- [ ] A mismatched `location` is detected and surfaced; an unreachable or older API
      does not fail a Run.
- [ ] The process-cwd path still works when the API is unavailable.

## Out of Scope

- Fixing opencode's own directory handling (ticket 05 - upstream).
- Stopping a Run (ticket 37).
