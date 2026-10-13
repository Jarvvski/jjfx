# Move `Store::reload` off the UI thread

Status: ready-for-agent
Type: bug
Blocked by: none

## Problem

`Store::load` opens the Repository and projects `jj workspace list`
(`repository.workspaces().projection()`), so it shells out. `Store::reload`
calls it, and every `App::reload()` plus the `Msg::Reload` branch of the event
loop calls that **inside the fold**:

- `Msg::Reload` is set by the filesystem watcher, so every `jj` command a user or
  an agent runs triggers a `jj workspace list` subprocess on the UI thread.
- `App::reload()` has five call sites (after create/delete/lift/tidy), each
  blocking the fold.

This is the remaining path from ticket 17's problem statement. It was split out
because the fix changes *when* the UI reflects disk state: today a reload is
synchronous and immediately visible, and afterwards it is eventually consistent
(the store swaps when the background load reports back).

## Fix

Route the load through the effect seam:

1. `Effect::ReloadStore`; the runtime runs `Store::load` on `spawn_blocking` and
   reports `Msg::StoreReloaded(Box<Store>)`.
2. `Msg::Reload` and `App::reload()` request the effect instead of loading.
3. The `Msg::StoreReloaded` handler replaces the store, calls
   `after_store_changed()`, and redraws.
4. Keep the selection and any pending tombstone stable across the swap - the
   existing `restore_pending_deletion`/`ensure_selection` calls are the seam.

## Acceptance Criteria

- [ ] No `jj` subprocess runs on the UI thread for a reload.
- [ ] A burst of watcher events collapses into one reload (the event loop already
      batches `Msg::Reload`; keep that property).
- [ ] The list still reflects a create/delete/tidy promptly.
- [ ] `mise run check` is green.

## Notes

- `Store` must be `Send` to cross the channel; it holds paths and plain data, so
  this should already hold.
- Tests that fold `Msg::Reload` and assert `app.store` synchronously will need to
  drive the effect and await `Msg::StoreReloaded`.
