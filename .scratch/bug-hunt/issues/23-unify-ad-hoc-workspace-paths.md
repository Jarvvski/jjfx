# Unify the ad-hoc workspace path rule

Status: ready-for-agent
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

- [ ] One rule is used by CLI and TUI, or both are pinned by tests.
- [ ] Existing workspaces are not orphaned by the change (or a migration is
      provided).
- [ ] `mise run check` is green.

## Out of Scope

- Moving existing on-disk workspaces.
