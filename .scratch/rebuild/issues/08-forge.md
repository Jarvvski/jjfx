# Forge a workspace (fetch -> weld -> push -> spr)

Status: done

## Parent

epics/C-forge-and-maintenance.md

## What to build

Native, workspace-safe forge (ADR 0005), porting `jj-forge`'s revsets
deliberately rather than shelling to the bash script: fetch, weld (rebase
`roots(mutable() & mine() & ::@)` onto trunk), push (`::@ ~ trunk() ~
conflicts()`), and PR sync via `jj-spr` (`JJ_SPR_REVSET` scoped to
`(::@ ~ trunk()) & mine()`). Each step is modeled as real step state feeding the
work axis - not scraped stdout. Includes the GPG-unlock guard and
conflict-skipping. `f` forges the selected workspace, `F` forges all, `g` forges
the default.

## Acceptance criteria

- [ ] `f` runs fetch -> weld -> push -> spr for the selected workspace, showing each step's live state; on success the work axis advances (Dirty -> Pushed/PrOpen).
- [ ] Workspace-safe revsets are used (weld scoped to `::@`), so forging one workspace never rebases another's chain.
- [ ] A conflicted workspace is skipped with a visible reason; a locked GPG signing key prompts to unlock and aborts cleanly if cancelled.
- [ ] `F` forges every eligible workspace sequentially; `g` forges the default; the existing `jj-forge` bash tool remains untouched and usable.

## Blocked by

- issues/05-work-lifecycle.md
- issues/07-workspace-actions.md
