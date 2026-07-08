# Commit graph rendered from jj-lib

Status: done

## Parent

epics/D-detail-views.md

## What to build

Render the jj commit graph from structured jj-lib data instead of scraping
`jj log --color always` (ADR 0007): per-workspace chains (`trunk()` plus each
workspace's mutable chain) and a "world" overview, with the selected workspace's
chain highlighted and optional freshness shading for recently-moved commits.

## Acceptance criteria

- [x] A graph panel shows `trunk()` and the workspaces' chains, built from commit data (not ANSI passthrough). (Verified live: `w` opens a full-screen world graph reading the store via `jj-lib`; each commit shows its jj change id, summary and bookmarks - `graph::load` walks the DAG by id, no `jj log` scraping. A per-workspace strip in the detail view shows one chain.)
- [x] Selecting a workspace highlights that workspace's chain in the graph. (Verified via ANSI capture: the selected `default` renders bold+cyan `38;5;6`, unselected `tester-workspace` bold only; selected-chain commit glyphs/ids brighten.)
- [x] The graph updates when the underlying revisions change (fetch, forge, new commits). (Verified live: with the world graph open, `jj new` made a fresh `@` appear at the head of the default chain with no interaction - the `.jj/` recursive watcher fires `Reload` -> `refresh_graph_if_visible`; `jj undo` reverted it. Forge also refreshes on `Done`.)
- [x] Rendering degrades gracefully in narrow terminals rather than corrupting layout. (Verified live at 40 cols: summaries elide with `…` and the box stays intact; the detail graph strip is dropped below `files+strip+40` width so the diff stays readable.)

## Blocked by

- issues/02-spike-jj-lib.md
- issues/03-skeleton-store.md
