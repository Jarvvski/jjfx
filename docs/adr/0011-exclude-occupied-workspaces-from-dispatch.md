# Exclude terminal-open Workspaces from Dispatch

Status: accepted

## Context

A Worker is dispatch-eligible when it is a Pool member and its Worker Status is
idle. Nothing else was consulted, so a Worker whose Workspace was open in a
terminal tab could be handed a new Run - colliding with the human sitting in
that tab. jjfx already knows which Workspaces are open (the `Terminal` seam
lists `jjfx:<name>` tabs), but that signal never reached the reservation path.

Two constraints shape where the check can live. The engine is frontend-neutral
and hides external commands (ADR 0009), and Worker reservation runs under the
Pool and per-Worker locks, so it must not shell out to the terminal. The
compatibility promise (`.scratch/wsg-migration/PRD.md`) forbids changing the
Go-compatible Pool and Worker documents, so occupancy cannot be a new field on
`WorkerState`.

## Decision

Introduce **Dispatch Eligibility**: a Worker is eligible when it is a Pool
member, its Worker Status is idle, and it is not **Occupied** (open in a
terminal). Only Pool Workspaces are ever eligible.

- **Occupied** is tracked by a jjfx-owned durable marker,
  `.jj/pool/in-use.json`, deliberately kept out of the compatibility documents.
  A compatibility peer ignores it.
- **The frontend computes the exclusion set; the engine enforces it.** A
  frontend queries the terminal, reconciles the marker against the open tabs,
  and passes a plain `DispatchExclusions` to the coordinator. The reservation
  path skips excluded Workers for implicit first-idle selection and refuses an
  explicitly chosen excluded Worker, all while holding the Pool lock. The engine
  never inspects the terminal.
- **Dispatch fails closed.** When the terminal cannot be queried, jjfx cannot
  tell which Workspaces are open, so Dispatch is refused rather than risking a
  collision. `--force` (CLI) overrides the check.
- **`send` respects occupancy; `review` does not.** A Follow-up prompt must not
  be injected into a Workspace a human is using, but `review` is how a waiting
  Worker is told to address its pull request, so it is exempt.

## Consequences

A Workspace open in a terminal is no longer idle capacity. The distinction is
queryable and enforced at one chokepoint, so CLI, TUI, orchestration, and
follow-ups all agree, and the Pool view surfaces it (an `[open]` tag and a
Dispatch preview that flags an open target). The durable marker lets a later
process know what jjfx opened, and reconciliation keeps it honest against tabs
opened or closed outside jjfx.

Fail-closed means a headless or off-kitty `dispatch` (no reachable terminal
socket) is refused until `--force`; this is a deliberate behaviour change.
Occupancy is a view of live terminal state, not a fourth lifecycle axis, so it
does not disturb the Agent, Work, or Worker Status axes.
