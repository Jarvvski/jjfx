# Import read-only Worker Pool snapshots into jjfx

Status: ready-for-agent

## Parent

epics/A-contract-and-coexistence.md

## Problem Statement

jjfx cannot currently identify Worker Workspaces or show Ticket, Run, Worker Status, alias, Agent Runtime, duration, or last-activity information from an existing wsg pool. The first integration should deliver value without risking writes from an incomplete Rust implementation.

## Solution

Implement read-only Repository, Worker Pool, and Worker state readers in the shared library. Produce immutable typed snapshots and feed them into jjfx's existing message loop. Enrich Workspace rows or a focused detail surface with Worker information while keeping every mutation disabled.

## Commits

1. Add typed identifiers and read-only models for Worker Pool, Worker, Run, Agent Runtime, and Worker Status.
2. Implement compatible readers for pool and Worker state fixtures.
3. Join Worker IDs to jjfx Workspaces without changing Workspace ownership.
4. Add a snapshot operation that reports missing or malformed state without partially inventing Workers.
5. Add a jjfx message carrying a complete Worker Pool snapshot.
6. Poll or watch relevant state files outside the App event loop and send changed snapshots only.
7. Render Worker alias, Ticket, Worker Status, Agent Runtime, and elapsed time in an incremental TUI surface.
8. Add a clear read-only indication so users cannot mistake displayed controls for active Rust mutation.

## Decision Document

- Snapshots are immutable values suitable for CLI and TUI callers.
- The shared library owns parsing; jjfx owns presentation and selection.
- A Worker Workspace remains a normal jjfx Workspace with additional execution metadata.
- Dead-PID reconciliation is reported as derived state only in this ticket; files are not rewritten.
- Malformed individual Worker state is surfaced without hiding healthy Workers.

## Testing Decisions

Test snapshot behavior through public readers using golden fixtures and temporary Repository layouts. Test jjfx message handling and rendering with synthetic snapshots. Assert that no file mtime or content changes during a read-only refresh.

## Acceptance Criteria

- [ ] jjfx displays a Go-created Worker Pool and its Worker assignments.
- [ ] Claude Code and Codex Workers deserialize correctly.
- [ ] Missing and malformed Worker files produce useful diagnostics.
- [ ] Read-only refresh changes no Repository file.
- [ ] Dead recorded PIDs do not remain visually busy, but are not persisted yet.
- [ ] `mise run check` is green.

## Out of Scope

- Pool or Worker mutation
- Launching Agent Runtimes
- Dispatch controls
- Replacing existing Workspace and Agent lifecycle displays

## Blocked by

- issues/03-lock-down-compatibility-contracts.md
