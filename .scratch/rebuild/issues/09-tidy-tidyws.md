# Maintenance: tidy, tidyws, and the behind indicator

Status: ready-for-agent

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

- [ ] A `tidyws` action rebases every idle, empty, undescribed workspace working-copy onto latest `trunk()` and zeroes its `behind` count; workspaces with real work are untouched.
- [ ] A `tidy` action (with confirmation) abandons only mutable, empty, undescribed, unbookmarked, untagged changes that are not `@`.
- [ ] Each workspace row or header can show how far behind trunk it is.
- [ ] The existing `jj tidy` / `jj tidyws` aliases remain untouched and usable standalone.

## Blocked by

- issues/02-spike-jj-lib.md
- issues/03-skeleton-store.md
