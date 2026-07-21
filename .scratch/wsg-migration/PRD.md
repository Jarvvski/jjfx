# wsg migration into jjfx - PRD

## Core idea

Reimplement wsg's Workspace Dispatch capabilities in Rust inside the jjfx repository. The implementation becomes a shared library used by both the jjfx TUI and a compatibility `wsg` binary built and released from the same Cargo workspace.

This is a fresh GPL-3.0-or-later implementation owned by jjfx. The Go repository is a temporary behavioral reference and compatibility peer during migration, not a linked dependency or a source library. Once parity and mixed-process compatibility are proven, the Go repository is deprecated in favor of the binaries released from jjfx.

## Why this belongs in jjfx

jjfx already owns the interactive Workspace, Agent, Session, Work, and Forge experience. Its accepted architecture explicitly left a Worker Pool and Ticket queue as a later layer over the same Workspace model. Bringing Workspace Dispatch into jjfx supplies that layer without forcing the TUI to invoke another application or independently reproduce wsg's lifecycle rules.

A shared Rust implementation gives both frontends one source of truth for:

- Repository and Workspace discovery
- Worker Pool capacity and Reservations
- Worker and Run lifecycle
- Agent Runtime invocation and Agent Session continuation
- Ticket discovery and Direct Dispatch
- Dispatch Group dependency progression
- persistent state, locking, and process reconciliation

## Product outcome

The jjfx repository produces two binaries:

- `jjfx`: the primary interactive mission-control TUI, extended with Worker Pool and Dispatch capabilities
- `wsg`: a script-friendly compatibility CLI over the shared Rust implementation

The `wsg` binary retains the existing command-oriented workflows. Running `wsg` without arguments enters the jjfx TUI rather than maintaining a second Rust TUI implementation.

## Compatibility promise

Migration is incremental. Go wsg and Rust wsg must be able to operate against the same Repository while components move. Rust must preserve:

- `.jj/ws-cache` byte format
- `.jj/pool.json` shape and optional fields
- Worker state JSON shape, including explicit `null` fields
- Dispatch Group JSON shape
- lock file names and lock scope
- atomic replacement behavior
- Worker Status and Sub-issue Status meanings
- PID and process-group reconciliation behavior
- stdout for machine-readable values and stderr for human messages
- existing command names, aliases, options, and meaningful exit behavior

No cutover may require destroying an existing Worker Pool or discarding an in-progress Dispatch Group.

## Domain model

The existing jjfx language remains authoritative for Workspaces, Agents, Sessions, Attention, Work, and Forge. Workspace Dispatch adds the following concepts:

- **Worker Pool**: the Repository-scoped collection of reusable Workers
- **Worker**: a reusable execution slot backed by one Worker Workspace
- **Run**: one execution attempt by an Agent Runtime in a Worker Workspace
- **Agent Runtime**: the external Claude Code or Codex program executing a Run
- **Ticket**: a Linear work item selected for implementation
- **Reservation**: capacity allocated to a Ticket before its Run starts
- **Dispatch**: routing a Ticket into execution
- **Direct Dispatch**: one Ticket assigned directly to one Worker
- **Dispatch Group**: dependency-aware progress for a Parent Ticket's direct Sub-issues
- **Dispatch Wave**: concurrently executable Sub-issues whose Dependencies are satisfied

A Worker is not an Agent, Session, Workspace, or process. An Agent Session can continue across Runs. A Worker Workspace is one kind of Workspace and remains visible through jjfx's existing Workspace model.

## Architecture

The jjfx repository becomes a Cargo workspace containing:

- the existing jjfx package and binary
- a shared Workspace Dispatch library
- a compatibility wsg CLI package and binary

The shared library presents deep interfaces for Repository access, Worker Pool operations, Dispatch, and events. It hides JSON files, locks, process groups, external commands, provider-specific logs, and retry mechanics. It does not render terminal output, parse CLI flags, or depend on Ratatui.

jjfx remains a single-owner application state driven by messages. Blocking filesystem and jj CLI operations run outside the async event loop. Long-running Agent Runtime processes report typed events back to callers.

Rust adoption does not imply moving every jj operation to jj-lib. ADR 0007 continues to govern jj-lib use. Existing CLI mutations remain CLI mutations unless a separate decision demonstrates that direct jj-lib use is stable and correct.

## Migration principles

1. Preserve observable behavior before replacing an implementation.
2. Introduce one deep module at a time behind a tested interface.
3. Keep both implementations operational until their shared surface has conformance coverage.
4. Make every implementation commit leave jjfx buildable and testable.
5. Test state transitions and external behavior, not private function arrangement.
6. Do not recreate the Bubble Tea TUI. Extend jjfx's Ratatui TUI instead.
7. Do not deprecate the Go repository until the Rust release supports existing state without destructive migration.

## Epics

| Epic | Outcome |
| --- | --- |
| A - Contract and coexistence | Domain decision, Cargo workspace foundation, compatibility fixtures, and read-only visibility |
| B - Worker Pool and runtime | Safe persistence, Workspace lifecycle, Agent Runtime execution, logs, Sessions, and Worker actions |
| C - Dispatch and orchestration | Linear discovery, Direct Dispatch, Dispatch Groups, and restart-safe orchestration |
| D - Interfaces and cutover | wsg CLI parity, jjfx controls, conformance, release, and Go repository deprecation |

## Success criteria

- jjfx can inspect and control an existing Go-created Worker Pool.
- Go and Rust processes honor the same locks and never corrupt shared state.
- Rust can resume an in-progress Dispatch Group created by Go.
- Both Claude Code and Codex Runs launch, finalize, reset, and resume correctly.
- `wsg` command workflows are available from a binary released by jjfx.
- jjfx exposes Worker Pool, Dispatch, logs, Send, Review, and Reset without duplicating domain rules.
- Cross-implementation conformance and end-to-end process cleanup tests pass.
- The Go repository is deprecated only after the Rust release is installed and documented.

## Non-goals

- Porting Go source code mechanically into Rust
- Preserving the Bubble Tea implementation
- Replacing gh, Agent Runtime, kitty, or every jj CLI call with an in-process library
- Redesigning the persisted wire formats during migration
- Supporting Windows before the current Unix process contract is preserved
- Removing the Go repository or its history
