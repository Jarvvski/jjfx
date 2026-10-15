# Promote conflicts to a first-class Work/Attention state with resolve

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

In jj, a conflict is stored **in the commit** and a rebase that hits one succeeds
and records it — a conflicted rebase is a normal, recoverable state, not a
failure. jjfx treats it as a reason to skip: the forge push revset excludes
conflicts (`PUSH_REVS = "::@ ~ trunk() ~ conflicts()"`, `src/forge.rs:37`) and the
push is gated on `(::@ ~ trunk()) & conflicts()` (`:321`). The review prompt
handles a `CONFLICTING` PR (`src/engine/worker_actions.rs:1070`). But there is no
conflict **state** in the Work/Attention model and no resolve affordance, so a
conflicted Workspace just silently fails to forge.

This is a jj-native capability no git-worktree rival can match, and it is
currently a blind spot.

## Solution

Surface a conflicted commit in a Workspace's change chain as a **needs-you** Work
state, name the conflicted commit, and offer resolve:

- Work lifecycle: add a conflicted state (or an Attention needs-you overlay) that
  names the commit id.
- Render it in the Workspace list with the blocked treatment.
- Resolve affordance: `jj resolve` (0.46 gained a built-in editor tool) or the
  `jj new <commit>` + edit + `jj squash` path, exposed from the CLI and the TUI.

## Commits

1. Detect conflicted commits in a Workspace's chain and represent them as a Work
   state (not a silent skip); classify from machine-readable jj state, per
   bug-hunt ticket 21.
2. Surface the state as needs-you in the Attention collapse and the list.
3. Add a resolve affordance (CLI command + TUI keybinding) using `jj resolve`.
4. Tests (a repo with a real conflicted rebase); docs; changelog.

## Acceptance Criteria

- [ ] A Workspace whose chain contains a conflict reads as needs-you and names the
      conflicted commit.
- [ ] The forge still refuses to push a conflicted commit, but the reason is the
      surfaced state, not a silent exclusion.
- [ ] A conflicted commit can be resolved from the tool.
- [ ] `mise run check` is green.

## Out of Scope

- Automatic conflict resolution (the agent or the human resolves).
- `jj resolve`'s editor UX beyond launching it.

## References

- jj conflicts: https://docs.jj-vcs.dev/latest/conflicts/
- `src/forge.rs:37,321`, `src/engine/worker_actions.rs:1070`, `CONTEXT.md` (Work lifecycle)

## Comments

- 2026-10-15: Filed from the strategy review.
