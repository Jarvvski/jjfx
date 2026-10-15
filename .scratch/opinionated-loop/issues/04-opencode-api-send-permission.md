# API-native `send`, follow-up, and permission resolution for opencode

Status: ready-for-agent
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

- [ ] An opencode `send` reaches the recorded session via `prompt_async` and does
      not depend on a PTY.
- [ ] A pending permission request can be answered from the TUI and clears
      `NeedsAttention`.
- [ ] When the API is unavailable, behaviour falls back to the existing path and
      never fails a Run.
- [ ] claude/codex are unchanged.
- [ ] `mise run check` is green.

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
