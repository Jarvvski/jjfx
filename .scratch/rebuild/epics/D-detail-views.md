# Epic D - Detail views

Type: epic
Status: tracking
Milestone: v0.3

## Goal

The rich progressive-disclosure detail for a selected workspace (ADR 0007/0008):
syntect-highlighted diffs and a natively-rendered commit graph built from
structured jj-lib data rather than scraped CLI output.

## Entry preconditions

- **11-graph** needs only Epic A (`02-spike-jj-lib`, `03-skeleton-store`).
- **10-diff-detail** needs `03-skeleton-store` (Epic A) and `05-work-lifecycle`
  (Epic B), so it waits for the work axis.

## Execution order

The two tickets are independent of each other; work them in either order or in
parallel once their gates clear.

1. **11-graph** can start as soon as Epic A is done.
2. **10-diff-detail** once `05-work-lifecycle` is done.

## How to work it

- Read ADR 0007 (why native jj-lib + syntect, not scraping / `bat`) and ADR 0008
  (progressive disclosure) before coding.
- Follow the landing gate in `CLAUDE.md`; user-visible, so bump the version + add
  a dated `CHANGELOG.md` entry in the landing commit.
- Set each ticket's `Status:` to `in-progress` / `done` as you go.

## Definition of done

- Selecting a workspace shows its changed files and a syntect-highlighted diff
  from trunk, with scroll + fuzzy filter and no `bat` process.
- A commit graph renders from jj-lib data, highlights the selected workspace's
  chain, and updates on revision changes.
- `mise run check` is green.
