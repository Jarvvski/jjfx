# Changelog

All notable changes to this project are documented here (newest first). The version of record lives in the project manifest.

## [Unreleased]

### Added

- 2026-07-08 - Work lifecycle (v0.3.0): each workspace row now also shows its
  work state - clean / dirty (with +/- LOC from trunk) / pushed / pr#N (with
  review verdict) / merged. jj state is read via CLI `-T` revsets relative to
  whatever `trunk()` resolves to (never assumed `main`); PR state comes from
  `gh --json`, with the PR derived by matching its head branch to a bookmark on
  the workspace's own change chain. A background poller refreshes every 15s and
  on any repo change; a missing `gh` or jj read degrades the row to unknown
  rather than crashing.

- 2026-07-08 - Agent lifecycle (v0.2.0): each workspace row now shows its live
  agent state - working / waiting / needs-attn / ended - event-sourced from
  Claude Code hooks. `jjfx hooks install` adds a dumb append-only hook to
  `~/.claude/settings.json` (idempotent, non-destructive) that appends each
  event to a global JSONL log; `jjfx hooks status` reports whether it is
  installed. The TUI replays the log on startup to reconstruct state, tails it
  for live transitions keyed by `cwd`, and bounds log growth with size-based
  rotation.

- 2026-07-08 - Walking skeleton: `jjfx` launches in a jj repo and renders a
  keyboard-driven workspace list (default + named). It reconciles the
  authoritative in-memory store from jj plus `.jj/ws-cache`, writes the cache
  through atomically (`name\tpath`) so the bash tools stay consistent, and
  watches `.jj/` so a shell-created workspace appears without a restart. `q`/esc
  quit and a restore-on-panic guard keeps the terminal intact. `--list` dumps the
  reconciled store headlessly.
