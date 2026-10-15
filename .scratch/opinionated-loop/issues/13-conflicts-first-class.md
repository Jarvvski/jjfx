# Promote conflicts to a first-class Work/Attention state with resolve

Status: resolved
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

- [x] A Workspace whose chain contains a conflict reads as needs-you and names the
      conflicted commit.
- [x] The forge still refuses to push a conflicted commit, but the reason is the
      surfaced state, not a silent exclusion.
- [x] A conflicted commit can be resolved from the tool.
- [x] `mise run check` is green.

## Out of Scope

- Automatic conflict resolution (the agent or the human resolves).
- `jj resolve`'s editor UX beyond launching it.

## References

- jj conflicts: https://docs.jj-vcs.dev/latest/conflicts/
- `src/forge.rs:37,321`, `src/engine/worker_actions.rs:1070`, `CONTEXT.md` (Work lifecycle)

## Comments

- 2026-10-15: Filed from the strategy review.
- 2026-10-15: Done. A conflicted chain is its own `WorkState::Conflicted { commit }`,
  read from jj's `conflicts()` revset over the whole chain rather than the owned
  subset, so the surfaced state and the forge's refusal agree by construction
  instead of by parsing jj's status prose (bug-hunt ticket 21's rule).
  - The state names the **owner** - the deepest conflicted commit - because
    `conflicts()` matches every commit that *inherits* the conflict, and
    resolving the owner clears the rest. It names the short **change id**, not
    the commit id: the commit id is rewritten the instant the conflict is
    resolved, while the change id is what `jj resolve -r` takes. The ticket said
    "names the commit id"; this is the same intent, with the identity that
    survives the resolve.
  - A conflicted chain leads needs-you **even while an agent is mid-turn**,
    unlike the changes-requested rule. A conflict blocks the forge outright, so
    the human has to decide whether to resolve it or hand it to an agent.
  - The forge refuses by name (`conflict in <change> - resolve it before
    pushing`) and re-checks after `jj fix` as well as after the weld, so a
    conflict a formatter buries is refused rather than quietly dropped by the
    push revset's `~ conflicts()`. That revset exclusion is kept as a second line
    of defence, but it is no longer the only thing standing between a conflict
    and a push.
  - Resolve is `jjfx resolve <workspace> [--tool <name>]`: `jj resolve -r
    <change>` deepest-first, re-reading after each one and stopping as soon as
    the set stops shrinking, so a merge tool that exits without changing anything
    cannot spin; then `jj workspace update-stale`, because resolving a commit
    below `@` leaves the working copy stale. `--tool` passes through, including
    jj's `:ours`/`:theirs`.
  - The merge tool is interactive, so the TUI keybinding (`C`) opens a tab
    running the same CLI command rather than hosting the tool in jjfx's own
    alternate screen - jjfx cannot give it a terminal. Rejected the `jj new
    <commit>` + edit + `jj squash` path the ticket offered: `jj resolve` is the
    one primitive that resolves *that* commit in place and rebases its
    descendants.
  - A Workspace whose commits another Workspace's rebase rewrote is **stale**,
    and jj refuses to resolve on a stale working copy, so the resolve brings it
    up to date first as well as after (`jj workspace update-stale`, a no-op when
    the working copy is current). CI caught this: the developer machine has
    `snapshot.auto-update-stale = true`, CI does not.
  - `docs/conflicts.md` records the read, the scoping rule and the resolve;
    `CONTEXT.md` gains a **Conflict** term.
  - Tests live with the commit they cover rather than all in the last one: the
    read against a real rebase conflict (`src/work.rs`, `src/jj.rs`), the list
    through the renderer (`src/render/home.rs`), the resolve end to end through
    the CLI (`tests/resolve.rs`), and the forge's refusal for a conflict the weld
    buries and one a formatter buries (`src/forge.rs`).
