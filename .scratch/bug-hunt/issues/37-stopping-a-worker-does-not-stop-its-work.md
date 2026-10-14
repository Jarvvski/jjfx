# Stopping a Worker does not stop its work

Status: resolved
Type: task
Blocked by: none

## Problem

Split out of ticket 05 (the Worker-cwd report's second finding).

`opencode run` without `--standalone` is a thin client of a long-lived shared
background service (`opencode serve --service`), so the process jjfx spawns,
records and later signals is **not** the process doing the work.

Consequences:

- `terminate_recorded_process` (`src/engine/runtime.rs:610`) and `reset_run`
  (`:573`) signal a client. The work continues: the report observed a Worker's shell
  tool call still running under the shared service minutes after the client was
  killed, and that Worker went on to open pull requests.
- `reconcile_worker` (`src/engine/pool.rs:350`) decides a Worker is finished from
  `process_is_alive_i64(pid)` - the *client's* liveness - so `jjfx pool list` can
  report a Worker `idle` while its work is still running.

## Why not `--standalone`

`opencode run --standalone` makes the run process host its own server, which fixes
ownership directly. **Rejected by the owner (2026-10-14):** one server per Run
multiplies CPU load across a pool, and a single shared server is preferred.

## What opencode actually exposes

The shared server's OpenAPI surface is reachable and has the primitives. Discovered
with `opencode api GET /openapi.json` (spec version `0.0.1`, "Experimental HttpApi
surface for selected instance routes"). None of the three are in the `experimental`
namespace:

| Operation | Route | Use here |
|---|---|---|
| `session.interrupt` | `POST /api/session/{sessionID}/interrupt` (optional `resume` query) | stop a running session |
| `session.active` | `GET /api/session/active` → `{data: {"ses…": SessionActive}}` | what is actually running |
| `session.get` | `GET /api/session/{sessionID}` | one session's state |

So a Run can be stopped **through the daemon**, which keeps the single shared server
the owner wants.

## The design question

Two things must change, and both need a decision before code moves.

### 1. How does jjfx identify the Run's session at teardown time?

jjfx already resolves a session id from the run log (`resolve_agent_session`,
`src/engine/run_log.rs:92`) for resumption, and exposes it as
`AgentSessionResolution` (`Fresh` / `Resumed { session_id }`) on the dispatch
result. Candidates:

- (a) **Re-resolve it from the run log at teardown.** Reuses the existing mechanism;
  no new persisted state. Costs a log read.
- (b) **Match against `session.active` by the `--title` jjfx already sets**
  (`pool:worker-<id>:<ticket>`). No log read, but titles are not guaranteed unique.
- (c) **Persist it in pool-owned sidecar state.** Explicit, but the Worker document
  is frozen by `docs/wsg-compatibility-contract.md:66-72`, so it cannot go there.

### 2. What does "stopping a Run" mean, and what is a Worker's liveness?

- **Stop:** `session.interrupt` for an opencode Run. pid/process-group signalling
  stays for runtimes that own their work (claude, codex, pi) and as the fallback when
  the API is unreachable (server down, or an opencode without the route).
- **Liveness:** `reconcile_worker` must not call a Worker finished while its session
  is active. Either consult `session.active`/`session.get`, or keep pid-based
  liveness only for the runtimes where the pid *is* the work.

The API describes itself as experimental and is versioned `0.0.1`, so the fallback
path matters and any adoption needs a capability check rather than an assumption.

## Acceptance Criteria

- [x] The chosen mechanism is recorded in `## Comments`.
- [x] Stopping a Worker stops its work, verified against a real daemon-hosted Run.
- [x] `jjfx pool list` does not report `idle` while a Run's work continues.
- [x] The pid/process-group path still works when the API is unavailable.

## Out of Scope

- The working-directory finding (ticket 05) - closed as an upstream opencode defect.
- `--standalone` - rejected; see ticket 07's 2026-10-14 note.

## Comments

**Decided 2026-10-14.**

- **Session identity - match by the `--title` jjfx already sets.** Find the Run's
  session by intersecting `session.list`'s titles with `session.active`, matching
  `pool:worker-<id>:<ticket>`. Rejected: re-resolving from the run log (a
  just-started Run may not carry its id yet, so it needs a fallback anyway) and
  minting the id up front (would mean minting ids in opencode's `^ses` namespace and
  changing how sessions are created).
  **Guard required:** never interrupt a stale session that merely shares the title.
  Restrict the match to active sessions, and if the match is ambiguous, do nothing
  rather than guess.
- **Stop semantics.** `POST /api/session/{id}/interrupt` for an opencode Run.
  pid/process-group signalling stays for the runtimes that own their work (claude,
  codex, pi) and as the fallback when the API is unreachable.
- **Liveness - session `outcome` or run-log result.** For an opencode Run a Worker is
  finished when the session has an `outcome` or the run log has a result; the
  pid/process-group liveness check is dropped for opencode, because the pid is only a
  client. Rejected: `session.active` (it reports a session as `running` while it is
  *open*, not while it is *executing* - it listed eight-plus sessions and cannot
  distinguish "finished" from "still open", so it would keep Workers `busy` after
  they finished), and "both outcome *and* dead pid" (a client that outlives its work
  would strand the Worker `busy`, which is the exact failure ticket 07 is about).

**Sequencing.** This changes `reconcile_worker` (`src/engine/pool.rs`) and
`terminate_recorded_process` / `reset_run` (`src/engine/runtime.rs`) - the same
functions tickets 07 and 20 change - so it must land **after** them, not alongside.

**Revision (2026-10-14, after ticket 38).** Ticket 38 has jjfx *create* the Run's
session via `session.create` and keep the id opencode returns, so jjfx knows the
session id outright and the title matching above is no longer needed. Revised:

- **Identify the session by the id jjfx created in 38.** Title matching is retained
  only as a fallback for a session jjfx did not create.
- Liveness stays `session.outcome` or a run-log result, now looked up by that id.
- **Sequence: 38 lands before 37**, both after 07/20.

The original choice is kept above for the record - it was right given what was known
then, and its rejection of *minting* an id still stands: 38 creates a session and
keeps the id opencode returns rather than inventing one.

**38 landed (2026-10-14).** The session id is now recorded in a jjfx-owned sidecar, `.jj/pool/sessions.json` (lock `.jj/pool/.sessions.lock`), per Worker, exposed as `StateStore::sessions()` / `SessionStateRepository`; it is overwritten by the Worker's next Run and never cleared. So identification at teardown is: read that sidecar for the Worker, and treat the Run as finished on the session `outcome` (or a run-log result) rather than on the client pid. Title matching stays only as the fallback for a session jjfx did not create. Note the id is recorded *before* the Run starts, so unlike re-resolving from the run log it is available from the first moment.

## Outcome (2026-10-14)

Landed as `Stop an opencode Run through its session and reconcile by outcome`. No new dependency and no `unsafe`.

**Stopping.** `RunSupervisor::stop_run` now decides how to end a Run: for an opencode Run it identifies the session and calls `session.interrupt` through ticket 39's client, and only a daemon-confirmed `{"interrupted":true}` counts as the stop. `terminate_recorded_process` (used by Worker `destroy`) tries the same daemon interrupt first. Everything else - the API unreachable or too old, no session identified, or `interrupted:false` - falls back to the existing `live_process_group_leader` + `terminate_process_group` path, and claude/codex/pi never take the daemon path at all.

**Liveness.** `reconcile_worker` splits on the persisted runtime. For opencode it consults the session: an `outcome` means finished (preferring the run log's provider result when it has one, otherwise mapping the outcome); with no outcome yet - or a session that cannot be read or identified - only a provider terminal event in the Run log finishes the Run, and anything less leaves the Worker `busy`. An open session is *not* treated as proof of a running one, because opencode reports a session as active while it is merely open, so the Run log stays a second signal rather than being skipped. The pid/process-group probe is dropped for opencode, where the pid is only a daemon client; claude/codex/pi keep the exact previous probe.

**Identification.** The exact id ticket 38 records in `.jj/pool/sessions.json` is used, with no API call. Only when nothing is recorded does `run_session_id` fall back to matching the Run title (`pool:<worker>:<ticket>`) against `session.list` restricted to `session.active`, and `match_active_session_by_title` returns an id only for exactly one match - none or ambiguous yields nothing, so a stale session sharing a title is never interrupted. The title format is now one helper, reused by direct dispatch.

**Evidence.** 7 unit tests through the `OpenCodeRunner` seam (active-session parsing; a recorded id wins with no API call; a unique active title match; an inactive match ignored; an ambiguous match refused; unavailable yields nothing; no id and no title yields nothing) and 4 integration tests in `tests/agent_runtime.rs` against a fake `opencode` on `PATH`: reset interrupts the recorded session and does not signal the client; reset falls back to the process group when the API is unavailable; reconcile keeps a busy Worker busy while its session is open even with a dead pid; and reconcile finishes it on the session outcome even with a live client pid.

**Verified live against opencode v2.0.29** (the acceptance criterion asks for a real daemon-hosted Run): a session was created and driven with `opencode run --session <id>` doing `sleep 300 && touch DONE`; `jjfx pool list` reported `busy`; the real stop path (`jjfx pool reset`) returned `{"interrupted":true}`, the `sleep 300` work was killed, the client exited on its own, `session.get` reported `outcome: "interrupted"`, no `DONE` marker was written, the Worker went idle, and the client pid was never signalled. Separately, a Run whose recorded pid *was* a live process-group leader reported `busy` while its session was open and `failed` after the session was interrupted directly through the API - so liveness follows the work, not the client.

**Correction to ticket 38's report.** opencode v2.0.29 *does* expose `DELETE /api/session/{id}` (`session.remove`); the 38 report recorded the opposite. The probe sessions were deleted with it (all four now 404), so nothing was left behind. `session.remove` was not added to the client - out of scope here.

Residual risk: the sidecar id is trusted as exact, per the decision. If a Run starts while the API is unavailable nothing is recorded and a *previous* Run's id lingers, so a later reconcile could read that stale session's outcome and finish the current Run early - for a stop this is benign (interrupting a finished session returns `false` and falls through), but for **liveness it is the hazard this ticket exists to remove**, and a staleness guard is the obvious follow-up. Reconcile now costs one API round-trip per busy opencode Worker on `pool list`/`dismiss`, inheriting ticket 39's no-timeout behaviour. A *successful* opencode Run emits no terminal log event, so with the API down and the owner gone the Worker stays `busy` - the ticket's deliberate trade-off, since a thin client that has exited says nothing about the work. `destroy`'s title fallback never fires (it has no ticket to match), so it relies on the sidecar id alone.
