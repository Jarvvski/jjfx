# Changelog

All notable changes to this project are documented here (newest first). The version of record lives in the project manifest.

## [Unreleased]

### Added

- 2026-07-09 - Target terminal (v0.14.0): a new config file,
  `${XDG_CONFIG_HOME:-~/.config}/jjfx/config.toml`, lets you host workspace
  sessions in a *different* kitty instance than the one jjfx runs in. Set
  `[terminal] listen_on` to that instance's `listen_on` base (e.g.
  `unix:/tmp/kitty-visor`) and jjfx routes every `kitten @` call there via
  `--to`, resolving kitty's live pid-suffixed socket (`/tmp/kitty-visor-<pid>`)
  from the base at call time so it survives restarts. Set `launch_command`
  (program + args) and jjfx runs it to start the target when its socket isn't
  found, then waits for it to answer. With no config jjfx behaves exactly as
  before, driving the surrounding kitty. A malformed config is reported at
  startup before the TUI loads; a missing one is fine.

- 2026-07-09 - Version flag (v0.13.0): `jjfx --version` (or `-V`) prints
  `jjfx <version>` and exits without discovering a jj repo or opening a terminal,
  so it is safe to run anywhere and lets `release`/CI smoke-test the built binary.

- 2026-07-08 - Lift onto trunk (v0.12.0): press `r` to rebase the selected
  workspace's own stack onto the current trunk, or `R` to lift every workspace at
  once - a local rebase, no push, that works whether the workspace is empty or
  carries work. This is the remedy for a `↓N` "behind" workspace: it rebases onto
  `TRUNK_BASE` (the same base `behind` and the dirty/clean check use), so lifting
  zeroes the `↓`. `tidyws` (`t`) now rebases onto that same `TRUNK_BASE` too, so it
  also clears the indicator; forge is unchanged (it still welds onto the remote
  mainline for clean PRs). Unlike `tidyws`, which only touches idle *empty*
  workspaces, `r`/`R` lift a workspace regardless of its contents.

### Changed

- 2026-07-09 - Workspace tab layout (v0.14.0): opening a workspace now builds
  three panes - claude on the left, and a right column split into two stacked
  shells - matching the `jj-wsx` layout. The splits are anchored to their window
  ids so they land correctly even when the tab is opened in the background, which
  now uses `--dont-take-focus` (so a background open never raises the target) and
  then focuses the claude pane.

### Fixed

- 2026-07-09 - Shared-base work state (v0.14.0): a workspace's state
  (PR / pushed / dirty) is now computed only from the commits it *owns*, not from
  a base it shares with other workspaces. Previously a branch sitting on a common
  ancestor - e.g. several workspaces parked on or stacked above one feature branch -
  made every one of them claim that branch's PR (and look pushed). Each commit is
  now attributed to at most one workspace: a commit on a single chain belongs to
  it, and a commit several workspaces share belongs only to the one that uniquely
  *heads* it (a base nobody uniquely heads is owned by none). So an empty workspace
  parked on a pushed branch reads `clean`, a workspace with its own commit on top
  reads `dirty` (measured from its own base), and normal stacked PRs still show.

- 2026-07-08 - Behind indicator (v0.11.1): the `↓N` "behind trunk" count now
  measures against the same base as the dirty/clean classification -
  `TRUNK_BASE` (the latest of the remote mainline and the local
  `main`/`master`/`trunk` bookmarks) - instead of jj's raw `trunk()`
  (`origin/main`). Previously, when local `main` was ahead of `origin/main`, a
  workspace could read `clean` yet show no `↓` despite being several commits
  behind the base its cleanliness was judged against.

### Added

- 2026-07-08 - Commit graph (v0.11.0): press `w` for a full-screen "world" graph -
  `trunk()` plus every workspace's chain, each commit shown with its change id,
  summary, and bookmarks, and the selected workspace's chain highlighted.
  Recently-moved commits are shaded brighter (freshness). The per-workspace
  detail view (`→`/`l`) gains a graph strip on the right showing just that
  workspace's chain from `trunk()` up to `@` plus one commit beyond it; the strip
  is dropped automatically on narrow terminals so the diff stays readable. The
  graph is read directly from the on-disk jj store via `jj-lib` (not by scraping
  `jj log`), and refreshes on its own as revisions change (new commits, fetch,
  forge). `j/k`/`PgUp`/`PgDn`/`g`/`G` scroll the world graph; `esc` closes it.

- 2026-07-08 - Diff detail view (v0.10.0): press `→`/`l` on a workspace to open a
  full-screen, two-pane detail - a changed-file list with per-file `+`/`-`
  magnitude bars on the left, the selected file's diff from `trunk()` on the
  right. The diff is highlighted in-process with `syntect` (no `bat` process),
  with the `+`/`-` gutters preserved and unknown languages degrading to plain
  text. Type to fuzzy-filter the file list; `↑`/`↓` pick a file; `→`/`tab` focus
  the diff and `j/k` + `PgUp`/`PgDn` scroll it; `esc` returns to the list. The
  diff is read on a background thread, and highlighting is incremental - each
  file is syntect-highlighted only as far down as the viewport has scrolled, so
  switching between files (or opening a large diff) never highlights the whole
  patch up front and navigation stays responsive.

- 2026-07-08 - Help overlay (v0.9.0): press `?` for a centered, bordered
  keybindings menu (action left, key right) drawn over the dimmed list; `?` or
  `esc` closes it. The footer no longer carries the full key list - in normal
  mode it now shows only `j/k move  ? help  q quit`.

### Fixed

- 2026-07-08 - Stop the forge pipeline from spewing over the TUI (v0.8.2): the
  `fetch`, `weld`, `push`, and `spr` steps ran their subprocesses with bare
  `.status()`, which inherits the parent's stdout/stderr - so jj/jj-spr output
  (`Working copy (@) now at:`, `Nothing changed.`, `Added 0 files, modified 5
  files`) printed straight onto the alt-screen and corrupted the workspace list.
  Those steps now redirect both streams to `Stdio::null()`, matching the forge's
  design of modelling step state natively rather than scraping stdout.

- 2026-07-08 - Correct the diff base in never-pushed repos (v0.8.1): `trunk()`
  resolves to the root commit when no remote mainline bookmark exists yet, which
  made every workspace diff the entire history - so an empty workspace read as
  `dirty` and landed in "ready to forge". The work snapshot now measures a
  workspace's chain and diff against `trunk()` when it is a real commit, else the
  local `main`/`master`/`trunk` bookmark. Once main is pushed, `trunk()` wins and
  behaviour is unchanged.

### Changed

- 2026-07-08 - Minimal restyle of the workspace list (v0.8.0): dropped the box
  border and the full-width reversed selection bar - the latter inverted every
  padded, coloured column into a solid block. Rows now carry a dim `·` bullet
  (the selected row a bright `▸`), only the selected name is boxed, and the
  attention column is padded past its widest heading so the columns line up
  across every row. Calmer and closer to a plain, minimal list.

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
