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

- [x] `f` runs fetch -> weld -> push -> spr for the selected workspace, showing each step's live state; on success the work axis advances (Dirty -> Pushed/PrOpen).
- [x] Workspace-safe revsets are used (weld scoped to `::@`), so forging one workspace never rebases another's chain.
- [x] A conflicted workspace is skipped with a visible reason; a locked GPG signing key prompts to unlock and aborts cleanly if cancelled.
- [x] `F` forges every eligible workspace sequentially; `g` forges the default; the existing `jj-forge` bash tool remains untouched and usable.

## Blocked by

- issues/05-work-lifecycle.md
- issues/07-workspace-actions.md

## Resolution

Reconciled Mon 19 Oct 2026 against the merged code at `9d6bd0ce6e7b`.

This ticket was marked `done` without its acceptance boxes ticked. Every criterion was re-verified by an independent read-only review of the code and tests on `main`. The reconciliation recorded no new work; it confirms what shipped.

Evidence: `src/forge.rs:1` documents the fetch -> weld -> push -> pr pipeline; `src/forge.rs:372` scopes the weld to the whole mutable chain and `:197` keeps it workspace-safe. `src/app.rs:956` binds `f`/`F`/`g`, `:2334` runs `F` sequentially in one background run, and `src/app.rs:709` folds live per-workspace `forge::Progress`. Tests: `forge::tests::one_workspace_reports_the_complete_pipeline_then_finishes_cleanly`, `locked_signing_key_aborts_before_fetch`, `conflicted_workspace_is_skipped_and_the_next_workspace_continues`, `pull_request_step_creates_and_links_the_stack`. The work axis is `Clean -> Dirty -> Pushed -> PrOpen -> Merged` (`src/work.rs:2`). The `jj-forge` bash tool is external to this repo and is not modified by the native port.
