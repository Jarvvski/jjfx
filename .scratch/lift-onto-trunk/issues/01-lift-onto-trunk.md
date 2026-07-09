# Lift a workspace onto trunk (local rebase, no push)

Status: done

## Parent

(standalone - follow-up to ticket 11 / the trunk-base consistency thread)

## Why

`behind` (the `↓N` indicator) measures against `TRUNK_BASE` (the latest of the
remote mainline and local `main`/`master`/`trunk`). Its only remedies were
`tidyws` (idle *empties* only) and forge (which pushes and welds onto the remote
`trunk()`), and both rebased onto jj's raw `trunk()` = `origin/main`. So when
local `main` was ahead of `origin/main`, nothing could lift a workspace onto the
base `behind` counts from, and `behind` never cleared. There was also no way to
"just rebase my workspace onto the latest mainline" locally without pushing.

## What was built

- `r` - lift the selected workspace's own mutable stack onto `TRUNK_BASE`
  (`jj rebase --skip-emptied -s 'roots(mutable() & mine() & ::<ws>@)' -d TRUNK_BASE`).
  Local, no push. Works empty or not. `-s` on the chain roots adapts to shape, so
  no `-r`/`-s` choice is needed.
- `R` - the bulk form, lifting every workspace (`::working_copies()`).
- `tidyws` now rebases onto `TRUNK_BASE` too (was raw `trunk()`), so tidying an
  idle empty workspace also zeroes its `↓`.
- Forge is deliberately unchanged: it still welds onto the remote `trunk()`
  (`origin/main`) so its pushed PRs stay based on the remote mainline and don't
  drag unpushed local-`main` commits to the remote as ancestors.

## Acceptance criteria

- [x] `r` lifts the selected workspace onto the trunk base and clears its `↓`.
- [x] `R` lifts every workspace in one rebase.
- [x] Works for a non-empty workspace, not just idle empties.
- [x] `tidyws` rebases onto `TRUNK_BASE`, matching `behind`/`classify`.
- [x] No push happens; forge's remote-oriented weld is untouched.
