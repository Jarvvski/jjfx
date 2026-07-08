# Diff detail with syntect highlighting

Status: ready-for-agent

## Parent

epics/D-detail-views.md

## What to build

Progressive-disclosure detail for the selected workspace (ADR 0007/0008): a
changed-files list (with +/- ratio bars) and a file diff from `trunk()`
highlighted in-process with `syntect` - replacing the original
`echo | bat | re-attach markers` approach and dropping the `bat` dependency.
Includes diff scrolling and a fuzzy file filter.

## Acceptance criteria

- [ ] Expanding/selecting a workspace shows its changed files with per-file +/- magnitude.
- [ ] Selecting a file shows its diff from `trunk()` with correct syntect highlighting and preserved +/- gutters; no `bat` process is spawned.
- [ ] Typing fuzzy-filters the file list; the diff view scrolls (line + page); large diffs do not block the render loop.
- [ ] Highlighting handles common languages in this repo (Rust, TOML, Markdown, shell) and degrades to plain text for unknown types.

## Blocked by

- issues/03-skeleton-store.md
- issues/05-work-lifecycle.md
