# Unify the ad-hoc workspace path rule

Status: resolved
Type: task
Blocked by: none

## Problem

Two code paths compute different on-disk locations for the same domain
operation (CONTEXT: "Ad Hoc Workspace: ... through the CLI or TUI"):

- CLI `jjfx add foo` -> `Workspaces::path` -> `<parent>/<repo>-workspaces/foo`
  (`engine/workspace.rs:180-186`).
- TUI create -> `store.rs:123` -> `engine::ad_hoc_workspace_path` ->
  `<parent>/<repo>-foo` (`engine/workspace.rs:529-535`).

CLI add/remove are self-consistent but disagree with the TUI and tombstone path.

## Fix

Pick one rule and use it in both paths, or document the divergence as
intentional with a test that pins each. The default rule is the sibling
`<repo>-workspaces/<name>` used by Workers and CLI add.

## Commits

1. Unify the path rule (or document + test the divergence).
2. Test: CLI-add and TUI-create produce the same path.
3. CHANGELOG entry `(fix)` (or `(break)` if an existing TUI path changes).

## Acceptance Criteria

- [x] One rule is used by CLI and TUI, or both are pinned by tests.
- [x] Existing workspaces are not orphaned by the change (or a migration is
      provided).
- [x] `mise run check` is green.

## Outcome (2026-10-13)

**Resolved.** `ad_hoc_workspace_path` (`engine/workspace.rs`) now delegates to
`workspace_base(root).join(name)` - the same helper behind `Workspaces::path` /
`base_dir` and `worker_path` - so the TUI, the CLI, and the Worker Pool share one
rule, and it honours `$JJ_WS_DIR` as they do.

`create_ad_hoc` also `create_dir_all`s the base directory before
`jj workspace add`: jj 0.46 does **not** create missing parents (verified
empirically), and the old rule always had an existing parent while the shared
base directory may not exist on a fresh repository.

**Existing workspaces are not affected - established from the code, not assumed.**
`Workspaces::projection` (`:210-238`) builds existence from jj's live name list
∪ the `.jj/ws-cache` `name\tpath` entries, and uses the **cached path when
present**, falling back to the naming rule only when there is no cache entry. TUI
creation writes the real path into that cache (`create_ad_hoc` →
`project_cache_entry`), and `Store::load` reads it. So an existing workspace keeps
its real on-disk path and only *new* creations move. `remove_ad_hoc` likewise
prefers the store's cache-backed `known_path`, and the pending tombstone derives
its path from the same `ad_hoc_workspace_path`, so tombstone and creation move
together by construction.

**CHANGELOG marker: `(fix)`, not `(break)`.** The ticket's rule is `(break)` only
"if an existing TUI path changes"; verified it does not (cache-preserved), so
this is a backwards-compatible unification. The entry notes that new TUI
creations now land under the shared base directory, which is the one observable
change.

**Test:** `tests/repository.rs::cli_add_and_tui_create_place_workspaces_in_the_same_base_directory`
creates one Workspace through `Workspaces::add` (CLI) and one through
`create_ad_hoc_workspace` (TUI), asserting both land under
`workspaces().base_dir()`. Pre-fix it fails with
`left: ".../T/.tmpmA1TzG-tui-feature"` vs
`right: ".../T/.tmpmA1TzG-workspaces/tui-feature"`. The tests that pinned the old
`<repo>-<name>` rule (path and teardown-marker locations) were updated with it.

**Residual risk (pre-existing, ADR-0006):** a *jj-only* workspace - created by a
raw `jj workspace add`, or one whose ws-cache entry was lost - has no cached path
and is projected through the naming fallback, so one created under the old TUI
rule would now project at the new path and read as pathless. jj does not record
workspace paths, so this limitation predates the change and affects only uncached
workspaces, not the normal cached TUI/CLI flows. On a failed create the new base
directory may be left empty, matching `Workspaces::add`'s existing behaviour.

## Out of Scope

- Moving existing on-disk workspaces.
