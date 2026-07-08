# Diff detail with syntect highlighting

Status: done

## Parent

epics/D-detail-views.md

## What to build

Progressive-disclosure detail for the selected workspace (ADR 0007/0008): a
changed-files list (with +/- ratio bars) and a file diff from `trunk()`
highlighted in-process with `syntect` - replacing the original
`echo | bat | re-attach markers` approach and dropping the `bat` dependency.
Includes diff scrolling and a fuzzy file filter.

## Acceptance criteria

- [x] Expanding/selecting a workspace shows its changed files with per-file +/- magnitude. (`→`/`l` opens a full-screen two-pane detail; the file list carries a scaled green/red `ratio_bar` per file. Verified live: bars track magnitude - `src/app.rs ████████` vs `src/main.rs █·······`.)
- [x] Selecting a file shows its diff from `trunk()` with correct syntect highlighting and preserved +/- gutters; no `bat` process is spawned. (Diff read via `jj diff --from TRUNK_BASE --to <ws>@ --git` - same base as the row LOC - then `syntect` highlights in-process; `+`/`-`/` ` gutters kept as coloured markers. PTY run confirmed 24-bit fg codes present, no subprocess but jj.)
- [x] Typing fuzzy-filters the file list; the diff view scrolls (line + page); large diffs do not block the render loop. (Type-to-filter subsequence match; `↑`/`↓` pick, `j/k` + `PgUp`/`PgDn` + `Ctrl-d/u` scroll. Diff is loaded on `spawn_blocking` and only the visible slice is cloned per render, so cost is bounded by the viewport.)
- [x] Highlighting handles common languages in this repo (Rust, TOML, Markdown, shell) and degrades to plain text for unknown types. (syntect `find_syntax_by_extension` with a `find_syntax_plain_text`-equivalent fallback; unit-tested that `.rs` yields multiple spans and an unknown `.zzz` renders one plain content span. Files past `MAX_HIGHLIGHT_LINES` also degrade to plain to protect the render loop.)

## Blocked by

- issues/03-skeleton-store.md
- issues/05-work-lifecycle.md
