# Maintenance: tidy, tidyws, and the behind indicator

Status: done

## Parent

epics/C-forge-and-maintenance.md

## What to build

Native versions of the two maintenance aliases (ADR 0005). `tidy` abandons junk
changes (`mutable() & empty() & description(exact:'') ~ @ ~ bookmarks() ~
tags()`); `tidyws` rebases idle empty workspace working-copies onto `trunk()`.
Also surface the `behind` indicator - how far `trunk()` has advanced past each
workspace's base - since tidyws is its remedy. Destructive `tidy` requires
confirmation.

## Acceptance criteria

- [x] A `tidyws` action rebases every idle, empty, undescribed workspace working-copy onto latest `trunk()` and zeroes its `behind` count; workspaces with real work are untouched.
- [x] A `tidy` action (with confirmation) abandons only mutable, empty, undescribed, unbookmarked, untagged changes that are not `@`.
- [x] Each workspace row or header can show how far behind trunk it is.
- [x] The existing `jj tidy` / `jj tidyws` aliases remain untouched and usable standalone.

## Blocked by

- issues/02-spike-jj-lib.md
- issues/03-skeleton-store.md

## Resolution

Reconciled Mon 19 Oct 2026 against the merged code at `9d6bd0ce6e7b`.

This ticket was marked `done` without its acceptance boxes ticked. Every criterion was re-verified by an independent read-only review of the code and tests on `main`. The reconciliation recorded no new work; it confirms what shipped.

Evidence: `src/jj.rs:342` ports the `tidyws` revset (idle, empty, undescribed, excluding bookmarks/tags) and `:412` rebases onto `crate::trunk::as_revset()` - the same base the `behind` indicator measures - so tidying zeroes `behind`; `src/jj.rs:432` implements `tidy`. `src/app.rs:2171` gates `tidy` behind `Mode::ConfirmTidy` and `:2155` runs `tidyws`. The behind count is surfaced per row at `src/render/home.rs:182` with the colour cue at `src/render/style.rs:272`, backed by `src/work.rs:356` and the unknown-not-zero test `src/work.rs:1281`. Tests: `app::tests::tidyws_requests_the_sweep_as_an_effect`, `tidyws_reports_the_count_from_the_background_sweep`, `tidy_runs_only_after_confirmation_and_reports_the_count`, `capital_t_confirms_before_tidy_and_esc_cancels`. The `jj tidy` / `jj tidyws` aliases are external to this repo and are not modified by the native port.
