# Set the Run's session directory explicitly

Status: resolved
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

- [x] A Run's session is created with its Worker Workspace as `location`.
- [x] A Run driven from the wrong process cwd still executes in its Worker Workspace.
- [x] A mismatched `location` is detected and surfaced; an unreachable or older API
      does not fail a Run.
- [x] The process-cwd path still works when the API is unavailable.

## Out of Scope

- Fixing opencode's own directory handling (ticket 05 - upstream).
- Stopping a Run (ticket 37).

## Outcome (2026-10-14)

Landed as `Anchor an opencode Run to its Worker Workspace through a created session`. No new dependency and no `unsafe`.

`RunSupervisor::prepare_reserved_session` runs before the request is built and is a no-op for every runtime but opencode. For an opencode Run it calls `opencode_api::create_run_session` (built on ticket 39's `OpenCodeApi`, not a second client), which creates the session with `location.directory` = the Worker Workspace and the `title` jjfx already sets, then reads it back with `session.get` and compares the directory opencode actually stored - exact, with a `canonicalize` fallback for a symlinked spelling. The returned id is set on the invocation, so `opencode_command` adds `--session <id>` exactly as today, and the process cwd is still set as before.

Three outcomes, as the ticket asked: **unavailable** (no executable, no server, older opencode, unparseable output) leaves the invocation unchanged and never fails a Run; a **server refusal** or a **mismatched anchor** fails the Run with `RunSupervisorError::OpenCodeSession` naming both directories, after the Reservation is released - the "do not commit into the wrong worktree" guard; and a session reported without a location, or a read-back that becomes unavailable, still drives the id it created.

**Where the id lives** - the design point 37 depends on: a new jjfx-owned sidecar, `.jj/pool/sessions.json` (lock `.jj/pool/.sessions.lock`), mapping Worker to session id, exposed as `StateStore::sessions()` / `SessionStateRepository`. It reuses the existing atomic-write and lock machinery, is additive to the Go-compatible documents (which are frozen, so the id could not go there), and does not overload `in-use.json`, whose whole set is reconciled from the terminal. The id is recorded for both created and resumed sessions, and overwritten by the next Run on that Worker.

Evidence: 7 unit tests over the `OpenCodeRunner` seam (create anchors and drives; blank title omitted; unavailable falls back; a refusal surfaces; a differing anchor fails naming both paths; a failed read-back still drives; a location-less session tolerated), a sidecar test in `tests/state_repositories.rs`, and 4 integration tests in `tests/agent_runtime.rs` against a fake `opencode` on `PATH`: a real reserved Run creates the session, drives it with `--session`, runs in the Workspace cwd and records the id; an unreachable API runs on cwd with no `--session` and records nothing; an anchor mismatch fails the Run, spawns nothing and releases the Worker; a resumed Run drives and records the existing id without creating another.

Residual risk: a failure to *write* the sidecar fails the Run (fail-closed - an unstoppable Run is the hazard 37 addresses), which is jjfx-owned state rather than the opencode API; the id is not cleared on finalization, so a finished Run leaves it until the Worker's next Run overwrites it (deliberate - clearing would race a newer Run, and 37 gates on outcome); every opencode Run now costs one API round-trip before it starts, inheriting ticket 39's no-timeout residual; and the shapes were verified live against opencode v2.0.29, which stores the directory verbatim - hence the defensive canonical compare.
