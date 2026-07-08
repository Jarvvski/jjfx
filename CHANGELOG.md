# Changelog

All notable changes to this project are documented here (newest first). The version of record lives in the project manifest.

## [Unreleased]

### Added

- 2026-07-08 - Forge pipeline (v0.7.0): drive a workspace toward merge from the
  list. `f` forges the selected workspace, `F` forges every eligible one, and `g`
  forges the default - each running fetch -> weld -> push -> spr natively (ADR
  0005) with the workspace-safe revsets ported from `jj-forge` (weld scoped to
  `::@`, push excluding `trunk()`/`conflicts()`, `jj-spr` handed a scoped
  `JJ_SPR_REVSET`). Every mutating step runs in the workspace's own directory, so
  forging one workspace never rebases another's chain. The pipeline runs on a
  background task and streams real step state (`⚒ f✓ w✓ p… s·`) onto the row - not
  scraped stdout; a clean run clears the overlay and the work axis advances. A
  conflicted working copy is skipped with a visible reason, and a locked GPG
  signing key is detected up front and aborts cleanly (no pinentry inside the
  alt-screen) rather than corrupting the terminal. The `jj-forge` bash tool is
  untouched and still usable standalone.

- 2026-07-08 - Maintenance: tidy, tidyws, and the behind indicator (v0.6.0):
  native versions of the two maintenance aliases (ADR 0005). `t` runs `tidyws` -
  rebasing every idle, empty, undescribed workspace working-copy onto latest
  `trunk()` (non-destructive, no confirmation); `T` runs `tidy` after a
  confirmation - abandoning junk mutable empties (undescribed, unbookmarked,
  untagged, never `@`). Both report how many changes they touched and are no-ops
  when nothing is eligible. Each row now also shows how far behind `trunk()` its
  base is (`↓N`, highlighted once far enough behind to warrant a reset). The
  proven revsets are ported from the `jj tidy` / `jj tidyws` aliases, which
  remain untouched and usable standalone.

- 2026-07-08 - Attention triage (v0.5.0): the list is now organized around the
  derived Attention badge (ADR 0008). Each workspace shows one signal - needs
  you / working / ready to forge / idle - derived from its (agent, work) pair,
  and the list is grouped in that order with the idle group foldable (`c`).
  needs-you distinguishes a Waiting agent over a changes-requested PR from an
  idle Clean workspace. A live state change (agent Stop, PR review) re-sorts the
  workspace into the right group with no manual refresh; selection follows a
  workspace by name as it moves.

- 2026-07-08 - Workspace actions (v0.4.0): drive workspaces from the list -
  `n` creates one (a new `jj` workspace in a `<repo>-<name>` sibling dir,
  persisted to the ws-cache) and opens a kitty tab running claude beside a
  shell; `enter` focuses its tab (or opens one), `o` opens in the background
  without stealing focus, and re-opening focuses rather than duplicating; `d`
  deletes after a confirmation (closing the tab and forgetting the workspace),
  and the default workspace is undeletable. All terminal control goes through a
  `Terminal` trait (kitty-only for now), so the multiplexer is swappable.

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
