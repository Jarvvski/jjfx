# API-native `send`, follow-up, and permission resolution for opencode

Status: resolved
Type: task
Blocked by: none

## Problem

For an opencode Run, jjfx drives the agent through a PTY and clears
`NeedsAttention` only by mounting the terminal. opencode exposes deterministic
HTTP primitives that make both mechanical, and jjfx already has the client and
the session identity to use them:

- `src/engine/opencode_api.rs` — the typed, best-effort client (bug-hunt ticket 39).
- `.jj/pool/sessions.json` — the session id jjfx records per Worker (ticket 38).

The opencode server exposes `POST /session/:id/prompt_async` (send a prompt to a
session without waiting) and `POST /session/:id/permissions/:permissionID` (answer
a pending permission request). Today `send` is PTY-dependent and a permission
request requires a human to `mount` the Worker.

## Solution

Route opencode `send` / Follow-up through `prompt_async`, and let a human answer a
pending permission request from the TUI via the API. Keep the PTY path as the
fallback when the API is unreachable or too old, exactly as the stop/liveness work
(bug-hunt tickets 37/39) already does.

## Commits

1. Extend `opencode_api.rs` with `prompt_async` and permission-reply operations,
   with the existing unavailable-versus-rejected contract.
2. Route opencode `send` and Follow-up through `prompt_async`, falling back to
   the PTY when the API is unavailable.
3. Add a TUI affordance to resolve a pending permission from the Pool/Workspace
   view so `NeedsAttention` can be cleared without mounting.
4. Tests against a fake `opencode` on `PATH`; changelog.

## Acceptance Criteria

- [x] An opencode `send` reaches the recorded session via `prompt_async` and does
      not depend on a PTY.
- [x] A pending permission request can be answered from the TUI and clears
      `NeedsAttention`.
- [x] When the API is unavailable, behaviour falls back to the existing path and
      never fails a Run.
- [x] claude/codex are unchanged.
- [x] `mise run check` is green.

## Out of Scope

- Richer lifecycle signals (ticket 03) — but they compose: a resolved permission
  is one way `NeedsAttention` clears.
- Headless parity (ticket 05).

## References

- opencode server: https://opencode.ai/docs/server/
- `src/engine/opencode_api.rs`, `.jj/pool/sessions.json`
- `.scratch/bug-hunt/issues/37-stopping-a-worker-does-not-stop-its-work.md`,
  `.scratch/bug-hunt/issues/39-opencode-api-client.md`

## Comments

- 2026-10-15: Filed from the strategy review. Endpoints verified against the
  opencode server docs; the client seam already exists.
- 2026-10-15: Landed. `opencode_api.rs` gains `prompt_async` (`session.prompt`), `reply_permission` (`session.permission.reply`), and `pending_permissions` (`session.permission.list`), all on the existing unavailable-versus-rejected contract. An opencode `send`/Follow-up to a Worker whose Run session jjfx recorded is delivered straight to that session through the daemon - no client process, no terminal - and anything the API cannot do (unreachable, too old, rejected) falls back to the existing client invocation, so claude/codex/pi are untouched. The Pool view gains `p` to answer a pending permission (once/always/reject) without mounting, which is what lets `NeedsAttention` clear. Pinned by 5 client unit tests, 7 `agent_runtime` integration tests against a fake `opencode` on `PATH`, and 2 app tests.
- 2026-10-15: Endpoint names differ from the ticket's docs links. The installed opencode (v2.0.29) exposes `session.prompt` for the asynchronous prompt the docs call `prompt_async`, and `session.permission.reply` for the reply route, so the client uses the operation ids the running server actually has (the docs' `POST /session/:id/permissions/:permissionID` is the v1 route).
