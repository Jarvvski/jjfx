# Work lifecycle: jj and gh state on each row

Status: done

## Parent

epics/B-triage-and-actions.md

## What to build

The work axis (ADR 0003). Query jj (via jj-lib or the `-T` fallback from issue
02) to classify each workspace as Clean / Dirty / Pushed relative to `trunk()`,
and shell to `gh --json` to attach PrOpen state + review decision. Derive the PR
association from the workspace using the derived identity (gh login prefix). Show
the work state on each workspace row; refresh on cache/event changes and a
periodic gh poll.

## Acceptance criteria

- [x] Each workspace row shows its work state: Clean, Dirty (with +/- LOC from trunk), Pushed, or PrOpen (with review verdict). (Verified: `work.label()` renders `clean`/`dirty +A/-B`/`pushed`/`pr#N <verdict>`/`merged`; live PTY run shows the default row as `dirty` - correct, since `trunk()` resolves to `root()` in this un-pushed repo.)
- [x] PR association is derived (gh login / branch prefix), not hard-coded; trunk is read from `trunk()`, not assumed to be `main`. (Verified: a PR is matched only when its `headRefName` equals a bookmark on the workspace's own `trunk()..<ws>@` chain - `pr_association_matches_by_head_branch`; the repo slug is derived from jj's `origin` URL - `slug_from_ssh_and_https`; every jj query uses the `trunk()` revset.)
- [x] jj reads use the mechanism chosen in issue 02; gh failures degrade gracefully (row shows unknown, not a crash). (Verified: jj read via CLI `-T` revsets/templates - the fallback issue 02 sanctions for this ticket; `list_prs` returns `[]` on any `gh` failure and `classify` returns `Unknown` on any jj failure; `gh pr list` on this repo returns `[]` cleanly.)
- [x] Work state updates without a manual refresh when the underlying change or PR changes (within the poll interval). (Verified: a background poller recomputes every 15s on `spawn_blocking` and is nudged immediately whenever the `.jj/` watcher fires a reload, sending `Msg::WorkSnapshot`.)

## Blocked by

- issues/02-spike-jj-lib.md
- issues/03-skeleton-store.md
