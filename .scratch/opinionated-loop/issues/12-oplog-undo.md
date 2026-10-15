# Expose jj's operation log as an undo surface

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

Every jj mutation is snapshotted in the operation log, and jjfx uses none of it
(verified: no `op log` / `evolog` / `jj op` handling in `src/`). So when a forge,
weld, reset, or tidy does something the owner did not want, there is no in-tool
way to undo it — the owner must drop to a shell and reason about `jj op log`
manually. This is a trust gap in the tool that exists to drive jj.

## Solution

Expose the operation log as a first-class undo/audit surface, scoped carefully
because **jj's undo primitives are repository-wide, and jjfx drives many
workspaces at once** — a naive "undo" could clobber an unrelated workspace.

- Record the operation id before each jjfx-initiated mutation (forge, weld, reset,
  tidy) and offer "undo the last <step> on this Workspace".
- Choose the primitive deliberately: `jj op revert <op>` reverts one operation's
  effect (more targeted) versus `jj undo` / `jj op restore` (repository-wide).
  Record the choice and the scoping rule in `## Comments` before coding.
- Optionally use `--at-op` to inspect what an agent did during a Run.

## Commits

1. Decide and record the primitive and the scoping rule in `## Comments`.
2. Record the pre-mutation operation id on the jjfx mutation paths.
3. Add an undo affordance (CLI + TUI) that restores the affected Workspace and
   warns when the operation was repository-wide.
4. Tests; docs; changelog.

## Acceptance Criteria

- [ ] A jjfx-initiated forge/weld/reset can be undone from the tool.
- [ ] The undo never silently affects an unrelated Workspace; a repository-wide
      effect is either avoided or warned about explicitly.
- [ ] `mise run check` is green.

## Out of Scope

- A full op-log browser (jjui already does that).
- Undo of user-run jj commands jjfx did not initiate.

## References

- jj operation log: https://docs.jj-vcs.dev/latest/operation-log/
- `src/forge.rs`, `src/jj.rs`, `src/engine/pool.rs`

## Comments

- 2026-10-15: Filed from the strategy review. The cheap, high-value trust win:
  jj already records the state; jjfx just needs to surface it.
