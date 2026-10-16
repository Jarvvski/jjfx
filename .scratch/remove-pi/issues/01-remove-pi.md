# Remove the Pi Agent Runtime

Status: resolved

## Problem Statement

The owner runs opencode and no longer uses Pi. Pi support is live, tested code -
tickets 02-05, 07 and 08 of the `pi-support` effort all landed - but nothing
exercises it against a real Pi install any more: ticket 06 (manual acceptance)
is `wontfix`. Keeping it costs a standing tax on unrelated work and leaves one
third-party pin ageing unwatched.

**Decided 2026-10-14: keep it for now, and remove it in a dedicated change.**
This ticket is that change, filed so the decision is recorded rather than
forgotten. Nothing here is urgent.

## Why it is worth doing eventually

- **A future-work multiplier.** `AgentRuntime::Pi` has **38 match sites** the
  compiler will point at (22 in `src/engine/runtime.rs`, 4 in `src/app.rs`, 3
  each in `engine/ticket.rs`, `engine/run_log.rs`, `cli.rs`, and one each in the
  dispatch and worker-action paths). Every change to runtime selection, liveness,
  reconciliation, prompts, logs or worker actions has to consider a fourth
  runtime that nobody uses.
- **A pin that rots silently.** Direct Dispatch and Follow-up require the exact
  `pi-mcp-adapter@2.11.0`, and the compatibility contract names Pi 0.84.x. With
  no real-Pi acceptance, drift is invisible until a user hits it.

## Measured footprint (2026-10-14)

Removing the `Pi` variant from `AgentRuntime` and running
`cargo check --workspace --all-targets` produces **38 errors across 8 files** -
that is the *rewiring*, and the compiler enumerates all of it. The *deletion* is
larger and forced: once the variant is gone the Pi machinery is unreachable, and
`clippy -- -D warnings` fails on dead code.

- **~137 Pi sites** across 11 `src/` files.
- **~15 Pi types**: `PiProfileProbe`, `PiMcpAdapter`, `PiMcpAdapterManifest`,
  `PiDiscoveryHelper`, `PiLinearTool`, `PiToolCall`, `PiEvent`, `PiUsage`,
  `PiCost`, `PiMessage`, `PiSessionHeader`, `PiDispatchProfile`,
  `PiAgentDirectory`, `PiPackageManifest`, and a 40-reference
  `PiDispatchProfileError`.
- **Two transports that exist only for Pi**:
  - `src/hooks.rs` - Claude and Codex use dumb append-only JSON hooks, while Pi
    uses a jjfx-owned **auto-discovered extension** with its own install path.
  - `src/engine/ticket.rs` - the `JJFX_PI_LINEAR_HELPER` versioned JSON
    discovery protocol (Pi has no native Linear access).
- **~81 Pi test functions** across the test files, 6 fixtures under
  `tests/fixtures/pi`, and `tests/fixtures/compatibility/worker-done-pi.json`.
- **`docs/pi-workers.md`** (115 lines) and the README references to it.
- Pi's session directory `.jj/pool/pi-sessions`.

Not involved: **no dependency** (`rustix`'s `pty`/`stdio` features are for the
terminal handover, ticket 20) and **no runtime cost to other runtimes** (the
profile probe is constructed only inside the Pi dispatch path).

## Solution

1. Remove `AgentRuntime::Pi` and fix the 38 compiler-reported sites.
2. Delete the machinery that becomes unreachable - the probe, the Pi log and
   session parsers, the Pi event/tool/usage types, and the large error enum.
3. Delete the Pi hook transport from `src/hooks.rs`, **keeping Claude's and
   Codex's dumb JSON hooks working**.
4. Delete the `JJFX_PI_LINEAR_HELPER` discovery path, **keeping the shared
   discovery abstraction and Claude's native Linear path working**.
5. Delete the Pi tests, fixtures and `docs/pi-workers.md`, and the README
   references to it.
6. **Amend `docs/wsg-compatibility-contract.md`**, which currently states "Rust
   supports Pi 0.84.x as an additive Worker runtime" and pins Pi's session
   directory, tool allowlist and unsupported capabilities.
7. Record a `momo break` changelog entry: removing a runtime is an incompatible
   change.

**The one design point that needs care:** persisted state. The contract fixes
`pi` as a canonical Worker runtime value, so a pool carrying `"agent": "pi"`
will stop parsing. That must fail with a **clear, actionable message** naming the
removal - not a confusing parse error - and a Worker already recorded as `pi`
must reach a terminal, non-busy state rather than being stranded.

## Acceptance Criteria

- [ ] `AgentRuntime::Pi` is gone and no Pi-specific type, function, transport or
      environment variable remains in `src/`.
- [ ] Claude, Codex and opencode behaviour is unchanged; their hooks, discovery
      and dispatch paths still work.
- [ ] A persisted `"agent": "pi"` pool profile or Worker fails with a clear
      message naming the removal, and leaves no Worker stranded `busy`.
- [ ] The Pi tests and fixtures are deleted, and `docs/pi-workers.md` and its
      README references are removed.
- [ ] The compatibility contract is amended and remains internally consistent.
- [ ] `mise run check` is green.

## Out of Scope

- Deprecating Pi *without* removing it (dropping it from the README and
  `docs/pi-workers.md` as a recommended runtime and marking the profile
  unsupported for new work). That is the cheaper lever if the goal is only to
  shrink the tax - near-zero cost, no contract amendment, no breaking change.
- Deleting `.scratch/pi-support/`, which stays as the history of why Pi was
  added, and `.scratch/pi-support/manual-acceptance.md`, the record of the
  step-1 review.

## Comments

**Filed 2026-10-14.** Deliberately not scheduled: Pi is harmless today, and the
only real costs are the multiplier on future work and the ageing adapter pin. The
owner's decision was "keep it for now - make a ticket to remove", so this ticket
exists to hold that decision and the survey above rather than to ask for the work
now. If it is picked up, expect a large, deletion-heavy diff and a review pass.

**Resolved 2026-10-16.** The Pi Agent Runtime is gone: `AgentRuntime::Pi`, its probe, log/session parsers, event/tool/usage types, the `JJFX_PI_LINEAR_HELPER` discovery path, the Pi hook transport, the TUI's violet styling, and the whole `pi-mcp-adapter` preflight seam are removed. Claude, Codex and opencode are unchanged, and their hooks, discovery and dispatch tests still pass.

Persisted state is handled deliberately. `AgentRuntime::from_configured` rejects a `pi` value with `the Pi Agent Runtime (<value>) was removed; choose claude, codex, or opencode with \`jjfx pool profile <runtime>\``, so a pool profile or Worker recorded as `pi` fails with a message naming the removal rather than a bare parse error. `reconcile_runs` finalizes a Worker still recorded under `pi` as `failed` with that message instead of leaving it stranded `busy`. Both paths are covered by tests.

`docs/pi-workers.md` and its README link are deleted, as are the Pi tests and fixtures (including `tests/fixtures/compatibility/worker-done-pi.json`), and `momo break` records the incompatible change.

**Deviation.** Solution step 6 asks to amend `docs/wsg-compatibility-contract.md`, but that file (and `tests/compatibility_contract.rs`) was already deleted when the wsg compatibility binary was eliminated, so there is nothing left to amend. No surviving doc or fixture pins Pi, so the compatibility-contract criterion is satisfied vacuously.
