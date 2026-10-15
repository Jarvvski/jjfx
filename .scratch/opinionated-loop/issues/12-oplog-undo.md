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
- 2026-10-15: **Primitive and scoping rule decided before coding.** Probed both
  primitives on a two-operation mutation (a rebase of a two-commit chain, then a
  rewrite of one commit). `jj op revert <op>` reverses the named operation's
  effect but does **not** move any working copy back: after reverting the rebase,
  the Workspace's `@` still descended from the rebased commits and the pre-rebase
  commits came back as *additional* commits, so the Workspace was not restored.
  `jj op restore <checkpoint>` returned the repository and every working copy to
  exactly the recorded pre-mutation state, as one forward operation that is
  itself undoable.
  **Primitive: `jj op restore <checkpoint>`**, where `<checkpoint>` is the
  operation id jjfx records immediately *before* it begins a mutation (the id this
  ticket asks for). `jj op revert` is not used: it undoes a single operation,
  while a jjfx mutation is a sequence of jj operations (a forge is fetch -> weld
  -> fix -> push), and it leaves the Workspace's working copy on the rewritten
  chain. `jj undo` is not used: it is `jj op restore` to the previous operation,
  with the same repository-wide effect and no way to say what it would discard.
  **Scoping rule: refuse unless the mutation is still the repository's tip.**
  Alongside the checkpoint jjfx records the operation id that was the tip when the
  mutation *finished*. An undo runs only when the current tip is still that
  recorded end - i.e. nothing at all has happened since, in any Workspace - so the
  operations between the checkpoint and the tip are exactly jjfx's own mutation
  and restoring to the checkpoint cannot discard another Workspace's work. When
  any operation has happened since (another Workspace's forge, an agent's commit,
  a fetch), jjfx **refuses** and reports how many later operations a restore would
  discard; `--force` is the explicit, warned override. The undo is per Workspace
  ("undo the last weld on ws-a"); a repository-wide jjfx mutation (tidy, tidyws,
  lift-all) is recorded under the reserved Workspace key `*` and never mixed with
  a single Workspace's record. Known residual risk, stated rather than hidden: jj
  does not record who created an operation, so an operation created by another
  process *inside* a jjfx mutation's own window is indistinguishable from jjfx's
  and a restore would discard it. jjfx narrows the window to one mutation and
  refuses on anything later; closing it entirely needs jj to tag operations.
