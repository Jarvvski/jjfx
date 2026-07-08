# Epic C - Forge and maintenance

Type: epic
Status: done
Milestone: v0.2

## Goal

Native, self-contained write pipelines that advance and maintain workspaces
(ADR 0005): the forge (fetch -> weld -> push -> spr) modeled as real step state,
and the tidy/tidyws maintenance operations. The existing bash tools stay alive
and untouched.

## Entry preconditions

Mixed - the two tickets have different gates, so this epic is not a single cold
start:

- **09-tidy-tidyws** needs only Epic A (`02-spike-jj-lib`, `03-skeleton-store`),
  so it can start as soon as A is done.
- **08-forge** needs Epic B (`05-work-lifecycle`, `07-workspace-actions`), so it
  waits for B.

## Execution order

1. **09-tidy-tidyws** first if Epic B is not yet done (it only needs A).
2. **08-forge** once Epic B is complete.

(Order is not strict - both can proceed the moment their own `## Blocked by`
gates clear.)

## How to work it

- Read ADR 0005 and study the *workspace-safe* revsets in the existing `jj-forge`
  / `jj tidy` / `jj tidyws` before porting - copy their scoping deliberately.
- Follow the landing gate in `CLAUDE.md`; these are user-visible, so bump the
  version + add a dated `CHANGELOG.md` entry in the landing commit.
- Set each ticket's `Status:` to `in-progress` / `done` as you go.
- Verify against the bash originals: forging/tidying via jjfx should produce the
  same repo state the bash tools would, and the bash tools must remain usable.

## Definition of done

- Forging a dirty workspace advances it to Pushed/PrOpen with live step state;
  conflicts and locked GPG keys are handled; `f`/`F`/`g` work.
- `tidyws` resets idle empty workspaces onto trunk; `tidy` sweeps junk empties;
  the `behind` indicator is shown.
- The `jj-forge`, `jj tidy`, `jj tidyws` originals are untouched and still work.
- `mise run check` is green.
