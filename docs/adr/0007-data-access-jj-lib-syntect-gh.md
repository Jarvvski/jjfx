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

Status: accepted
