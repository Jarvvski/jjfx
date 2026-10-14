# A typed client for opencode's server API

Status: ready-for-agent
Type: task
Blocked by: none

## Why

Tickets 37 and 38 both need to call opencode's server API and understand the same responses: 38 to create a Run's session with an explicit `location`, and 37 to find that session and interrupt it. Without a shared client each would grow its own copy, so this extracts it first — it touches no file that 07/20 is changing, so it can be built in parallel.

## What is needed

The operations both tickets use, verified live against opencode v2.0.29:

| Operation | Route | Used by |
|---|---|---|
| `session.create` | `POST /api/session` | 38 — sets `location.directory` |
| `session.get` | `GET /api/session/{sessionID}` | 37, 38 — confirm `location`, read `outcome` |
| `session.list` | `GET /api/session` | 37, 38 — find a session |
| `session.interrupt` | `POST /api/session/{sessionID}/interrupt` | 37 — stop a Run |

Reached via the `opencode api` CLI (`opencode api <operationId> [-d <json>]`), which is the chosen approach: it needs no new dependency, and jjfx already drives `jj` and `gh` the same way.

## Design points to settle in the code

- **Best-effort by contract.** The API describes itself as experimental (spec `0.0.1`). Every call must be able to report "unavailable" distinctly from "the server rejected this", so callers can fall back rather than fail. Both 37 and 38 require that an unreachable or older opencode never fails a Run.
- **Typed responses.** Represent the session (`id`, `location.directory`, `outcome`, `title`) rather than passing raw JSON around.
- **One seam for tests.** The module must be testable without a live server: the command runner and its output have to be injectable, in the same spirit as the cleanup-outcome seam ticket 20/C6 added.

## Acceptance Criteria

- [ ] The operations above are wrapped with typed request/response values.
- [ ] "Unavailable" is distinguishable from "rejected" by callers.
- [ ] Tests cover parsing and the unavailable path without a live server.
- [ ] No new dependency; no `unsafe`.

## Out of Scope

- Calling the client from the Run path (tickets 37 and 38).
- opencode's own directory handling (ticket 05 — upstream).
