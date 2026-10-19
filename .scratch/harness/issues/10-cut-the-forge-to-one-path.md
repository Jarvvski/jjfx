# One blessed forge path, and take the forge out of the prompt

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

The direction is agent-deep, which means subtraction. The forge is currently a
wide surface - base-chained stack assembly, a review-bot ingestion path, a
`gh pr list` model, and a set of landing verbs - and, less obviously, it is
**baked into the dispatch prompt**: `dispatch_prompt.rs:150-237` instructs the
agent to run `jj git push`, `gh pr create` and Linear status moves, and
`direct_dispatch.rs:608-658` constructs the delivery command.

So shrinking the forge is not a deletion; it is a prompt change plus a module
cut. The blessed path to keep is the one the owner ruled: agents push, open a
PR, merge on green.

## Acceptance Criteria

- [ ] The forge is one path: push, open a PR, read the check, merge on green.
- [ ] `gh stack` assembly is cut (its spike already resolved to `wontfix`), and
      review-bot ingestion with it.
- [ ] The dispatch prompt no longer carries forge instructions it cannot rely
      on; delivery is described in terms of what jjfx does, not what the agent
      must remember to run.
- [ ] A repository with no forge is a supported, tested path rather than a
      degraded one.

## References

- `src/forge.rs`, `src/forge/pull_requests.rs`, `src/prs.rs`,
  `src/engine/reviewer.rs`
- `src/engine/dispatch_prompt.rs:150-237`, `src/engine/direct_dispatch.rs:608-658`
- `.scratch/opinionated-loop/issues/09-gh-stack-adoption.md` (wontfix),
  `10-opinionated-landing.md` (decision (a))
