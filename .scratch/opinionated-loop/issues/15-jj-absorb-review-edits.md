# Use `jj absorb` for review-feedback edits

Status: ready-for-agent
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

- [ ] Review feedback that targets a base commit lands there via `jj absorb`
      without manual splitting.
- [ ] The review prompt and any jjfx command agree on the flow.
- [ ] `mise run check` is green.

## Out of Scope

- Reviewer automation — ticket 08.
- General-purpose squash/split UIs.

## References

- `jj absorb`: https://docs.jj-vcs.dev/latest/cli-reference/#absorb
- `src/engine/worker_actions.rs:1052` (`build_review_prompt`)

## Comments

- 2026-10-15: Filed from the strategy review. jj-native; it removes hand-rolled
  squash targeting from the review loop.
