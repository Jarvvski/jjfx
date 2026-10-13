# Move `Store::reload` off the UI thread

Status: resolved
Type: bug
Blocked by: none

## Problem

`Store::load` opens the Repository and projects `jj workspace list`
(`repository.workspaces().projection()`), so it shells out. `Store::reload`
called it, and every `App::reload()` plus the `Msg::Reload` branch of the event
loop called that **inside the fold**:

- `Msg::Reload` is set by the filesystem watcher, so every `jj` command a user or
  an agent ran triggered a `jj workspace list` subprocess on the UI thread.
- `App::reload()` had five call sites (after create/delete/lift/tidy), each
  blocking the fold.

This was the remaining path from ticket 17's problem statement.

## Fix

Routed the load through the effect seam:

1. `Effect::ReloadStore`; the runtime runs `Store::load` on `spawn_blocking` and
   reports `Msg::StoreReloaded(Store)`.
2. `Msg::Reload` and `App::reload()` request the effect instead of loading.
3. `on_store_reloaded` swaps the store in, calls `after_store_changed()`, and
   redraws.

The event loop already collapses a burst of watcher events into one
`Msg::Reload`, so the batching property is preserved.

`Store::reload` was deleted: its only callers were `Store::create` and
`Store::delete`, which now reconcile inline. `Store::create` and `Store::delete`
are test-only paths, so leaving a `pub(crate) fn reload` behind was dead code.

## The create-then-open path

`on_workspace_setup_completed` used to reload synchronously and then check
`self.store.workspace(&name)` to decide whether the Workspace had survived setup
before opening its tab. With an asynchronous reload the stale store would always
fail that check and the tab would never open.

The check now runs when the fresh projection lands: the App records
`pending_created_open: Option<(String, PathBuf)>` and
`on_store_reloaded` performs the existence check and then opens the tab (or
reports "setup finished after the workspace disappeared"). The semantics are
unchanged; only *when* the check runs moved.

## Commits

1. `Re-project the Workspace list on a background worker` - the effect, the
   message, the deferred create-then-open check, two tests, and a CHANGELOG
   `(fix)`.

## Acceptance Criteria

- [x] No `jj` subprocess runs on the UI thread for a reload.
- [x] A burst of watcher events still collapses into one reload (unchanged: the
      event loop batches `Msg::Reload` before folding it).
- [x] The list still reflects a create/delete/tidy promptly; the create tests now
      drive the reload and assert the tab opens from the fresh projection.
- [x] `mise run check` is green.

## Notes

- `Msg::StoreReloaded` carries the store unboxed, matching the existing
  `WorkspaceDeletionCompleted { store: Option<Store> }` variant.
- `reload_is_requested_as_an_effect_not_run_in_the_fold` asserts the fold only
  asks for the projection.
