# A typed client for opencode's server API

Status: resolved
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

## Outcome (2026-10-14)

Landed as `src/engine/opencode_api.rs`, re-exported from `src/engine/mod.rs` (the
only other file touched). No dependency and no `unsafe`.

Public shape, all under `jjfx::engine`:

- `OpenCodeApi<R = ProcessOpenCodeRunner>` — `create_session(&NewSession)`,
  `get_session(id)`, `list_sessions()`, `interrupt_session(id)`.
- `Session` — private fields with `id()`, `directory()`, `outcome()`, `title()`
  accessors, so callers never thread raw JSON around.
- `SessionOutcome` — `Succeeded | Failed | Interrupted | Unknown(String)`; an
  unknown wire value is preserved rather than failing the parse.
- `NewSession` / `ModelRef` for the create body.
- The seam: an `OpenCodeRunner` trait plus a constructible `CommandOutput`, with
  `ProcessOpenCodeRunner` as the production implementation.

The contract the ticket asked for is `OpenCodeError`: `Unavailable` (no executable,
no running server, an older opencode without the operation, output that will not
parse) versus `Rejected` (the server answered and refused, carrying the HTTP status
and its message). `is_unavailable()` / `status()` let 37 and 38 branch without
matching on internals, and an unreachable opencode can never fail a Run.

Evidence: 12 tests drive the client through a fake runner — parsing of
`session.list`/`get`/`create`, request-body construction, path-parameter argv,
unknown-outcome preservation, every unavailable path, and rejection with and
without the CLI's HTTP status line. All without a live server. The real
request/response shapes were confirmed against a running opencode v2.0.29
(list/create/get/interrupt, a genuine 404 rejection, and a bogus executable), and
the temporary probe example was deleted before committing.

Residual risk, recorded for 37/38: `Command::output()` has no timeout, so a wedged
server could block a caller (ticket 35 owns helper deadlines); and the
rejected-versus-unavailable split keys off the CLI's stderr/exit protocol, which is
not part of the experimental spec — if a future opencode changes it, calls degrade
to `Unavailable` (the safe fallback) rather than misreporting as `Rejected`.

No changelog entry: a library module with no user-visible behaviour change.
