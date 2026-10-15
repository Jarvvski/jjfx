# Use `jj absorb` for review-feedback edits

Status: resolved
Type: task
Blocked by: none

## Problem

Review feedback often belongs in a **base commit**, not a new one stacked on top.
Today that means hand-splitting or squashing to target the right commit — fiddly
work the review prompt pushes onto the agent. jj has `jj absorb`, which moves
working-copy hunks into the closest mutable ancestor where those lines last
changed, and abandons the source if fully absorbed. jjfx does not use it
(verified: no `jj absorb` in `src/`).

## Solution

Make `jj absorb` the primitive for "address this feedback in the base commit" in
the review flow: `jj new <commit>` → edit → `jj absorb` folds the edit into the
right ancestor automatically. Update the review prompt to prefer it over manual
squash/split, and add a jjfx affordance where it helps.

## Commits

1. Update the review-feedback guidance (`build_review_prompt`) to use
   `jj new <commit>` + `jj absorb` for base-commit feedback instead of manual
   squash/split.
2. Add a jjfx affordance/command that runs the absorb step where it is mechanical.
3. Tests; docs; changelog.

## Acceptance Criteria

- [x] Review feedback that targets a base commit lands there via `jj absorb`
      without manual splitting.
- [x] The review prompt and any jjfx command agree on the flow.
- [x] `mise run check` is green.

## Out of Scope

- Reviewer automation — ticket 08.
- General-purpose squash/split UIs.

## References

- `jj absorb`: https://docs.jj-vcs.dev/latest/cli-reference/#absorb
- `src/engine/worker_actions.rs:1052` (`build_review_prompt`)

## Comments

- 2026-10-15: Filed from the strategy review. jj-native; it removes hand-rolled
  squash targeting from the review loop.
- 2026-10-15: Resolved. The review prompt's "address each unresolved comment"
  step now says where a fix belongs before it is made: base-commit feedback goes
  through `jj log` to find the owning commit, `jj new <commit>` to start an edit
  on top of it, the edit itself, then `jj absorb` - explicitly *not* a new commit
  stacked on top and *not* a hand-split. It also warns that absorb can leave the
  commits above it conflicted and says what to do (`jj undo`, or address the
  feedback on `@`). `jjfx absorb <worker>` is the same absorb step from outside
  the workspace: `jj absorb` in the Worker's Workspace, then a push of the
  rewritten bookmark. It reads `jj absorb`'s stderr summary (absorb exits zero
  whether or not it moved anything), reports how many revisions took hunks and
  whether any hunks stayed behind, and skips the push when nothing was absorbed,
  so it is safe to run speculatively. A failed push rolls back with `jj undo`,
  matching `rebase`. `jjfx absorb` is the only jjfx command in this flow, and it
  runs exactly the `jj absorb` the prompt prescribes; both are pinned by tests.
  Verification: `the_documented_base_commit_flow_lands_feedback_in_the_base_commit`
  drives real jj through the documented sequence and asserts the base commit ends
  up with the fix, the descendant keeps its own change, and the edit commit is
  abandoned - the direct proof that no manual splitting is involved. Three
  integration tests cover the command's argv (absorb + push), the nothing-to-do
  case (no push), the hunks-stayed-behind case, and the push-failure rollback.
  Deviations worth noting: the command's tests landed with the command (commit 2)
  rather than in the tests/docs/changelog commit, since they pin its argv; and
  the prompt keeps to raw `jj` rather than naming `jjfx absorb`, so the Worker's
  agent needs no new binary on its PATH.
