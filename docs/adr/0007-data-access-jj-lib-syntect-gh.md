# Data access: jj-lib for jj, syntect for highlighting, gh for PRs

jjfx reads jj repository state through jj-lib (the Jujutsu Rust library) rather
than parsing CLI output, highlights diffs with syntect in-process (dropping the
`bat` dependency), and lists PRs by shelling to `gh --json`. jj-lib and syntect
give typed, in-process data on the two paths where that pays; `gh` is kept for
PRs because `gh --json` already returns structured data with auth fully handled,
and octocrab would add credential management for no real gain on "list my PRs".

## Consequences

- jj-lib has no stability guarantee and reads the on-disk `.jj/` store directly,
  so it must match the installed `jj` binary's store format. `jj` is therefore
  pinned in `mise.toml` to the version jj-lib targets and upgraded in lockstep -
  a `jj` bump becomes a jjfx change gated by `mise run check`. This is tolerable
  only because jjfx is a personal tool on a controlled machine.
- jjfx links a large dependency tree and is version-coupled to jj internals. In
  exchange, the graph is built from structured commit data instead of scraping
  `jj log --color always` output (as the original TUI did for its "world" view).

## Note (2026-07-08, ticket 11)

`jj-lib` is now a real dependency: the commit graph reads the on-disk store
directly through it (pinned to the `jj` version in `mise.toml`, 0.43.0). The read
surface is deliberately minimal - open the workspace, read heads/bookmarks/wc
commits, walk parents by id - and trunk resolution is reimplemented in typed Rust
mirroring `work::TRUNK_BASE` (real-remote `main`/`master` -> local -> root), so it
never touches jj-lib's revset engine (the churny, alias-config-dependent surface).

This changes the premise of the parked `jj-lib-reads` ticket, whose gate assumed
no jj-lib in the tree: adopting it here was scoped to the graph (a new read site),
not the five CLI reads that ticket covers. Those stay on the CLI `-T` path for now;
the ticket is re-triaged to `needs-triage` to reassess the gate against the churn
we actually observe on the next `jj` bump.

## Note (2026-07-09)

The trunk mirror this ADR accepts now lives in one place. The revset (CLI reads)
and the jj-lib walk (graph) were two hand-maintained encodings of the same
selection rule with nothing forcing them to agree - the missing agreement test.
Both now sit in a `trunk` module that states the rule once as an ordered `SOURCES`
list: `as_revset()` builds the revset string from it and `resolve()` gathers the
jj-lib candidates from the same list, so the two adapters cannot drift by
construction. The seam is genuine (two engines, one rule); a unit test pins the
SOURCES-derived candidate order against the revset's priority, and another pins the
revset string. `forge`'s weld/push target keeps jj's *bare* `trunk()` - the real
remote mainline you push against - as a named, documented exception, not a fourth
accident.

Status: accepted
