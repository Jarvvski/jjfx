# Commit graph rendered from jj-lib

Status: ready-for-agent

## Parent

epics/D-detail-views.md

## What to build

Render the jj commit graph from structured jj-lib data instead of scraping
`jj log --color always` (ADR 0007): per-workspace chains (`trunk()` plus each
workspace's mutable chain) and a "world" overview, with the selected workspace's
chain highlighted and optional freshness shading for recently-moved commits.

## Acceptance criteria

- [ ] A graph panel shows `trunk()` and the workspaces' chains, built from commit data (not ANSI passthrough).
- [ ] Selecting a workspace highlights that workspace's chain in the graph.
- [ ] The graph updates when the underlying revisions change (fetch, forge, new commits).
- [ ] Rendering degrades gracefully in narrow terminals rather than corrupting layout.

## Blocked by

- issues/02-spike-jj-lib.md
- issues/03-skeleton-store.md
