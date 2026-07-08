# Work lifecycle: jj and gh state on each row

Status: ready-for-agent

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

- [ ] Each workspace row shows its work state: Clean, Dirty (with +/- LOC from trunk), Pushed, or PrOpen (with review verdict).
- [ ] PR association is derived (gh login / branch prefix), not hard-coded; trunk is read from `trunk()`, not assumed to be `main`.
- [ ] jj reads use the mechanism chosen in issue 02; gh failures degrade gracefully (row shows unknown, not a crash).
- [ ] Work state updates without a manual refresh when the underlying change or PR changes (within the poll interval).

## Blocked by

- issues/02-spike-jj-lib.md
- issues/03-skeleton-store.md
