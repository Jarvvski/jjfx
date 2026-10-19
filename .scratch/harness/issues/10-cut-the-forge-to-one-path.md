# One blessed forge path, and take the forge out of the prompt

Status: resolved
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

- [x] The forge is one path: push, open a PR, read the check, merge on green.
- [x] `gh stack` assembly is cut (its spike already resolved to `wontfix`), and
      review-bot ingestion with it.
- [x] The dispatch prompt no longer carries forge instructions it cannot rely
      on; delivery is described in terms of what jjfx does, not what the agent
      must remember to run.
- [x] A repository with no forge is a supported, tested path rather than a
      degraded one.

## References

- `src/forge.rs`, `src/forge/pull_requests.rs`, `src/prs.rs`,
  `src/engine/reviewer.rs`
- `src/engine/dispatch_prompt.rs:150-237`, `src/engine/direct_dispatch.rs:608-658`
- `.scratch/opinionated-loop/issues/09-gh-stack-adoption.md` (wontfix),
  `10-opinionated-landing.md` (decision (a))

## Resolution

**The one path.** The forge is now a single path - fetch, weld, check, push,
open one Pull Request per change. "Read the check, merge on green" is the
surrounding loop's landing rule, not a second forge: jjfx runs the repo-owned
check before it pushes, and the merge on green is the agent's/owner's step
(10-opinionated-landing deferred an in-jjfx landing step as a future change).
The paths that made the forge wide - base-chained stack assembly and review-bot
ingestion - are the ones that went.

**What was cut.** The forge's base-chained stack assembly is gone:
`pull_requests::submit` no longer computes a PR's base from the bookmarks around
it (`base_for`) and no longer writes a `## Stack` section into a PR body
(`stack_section`, `merge_body`, `strip_stack_section`). Every change gets one
Pull Request based on the repository's default branch, or is left alone when it
already has one. The `GitHubAdapter` shrank to `default_branch`, `exists` and
`create` - `find`, `edit`, the `PullRequest` model and the `Pr::body` field (the
stack section's only reader) all went with it.

Review-bot ingestion is gone too: `src/reviewer_source.rs` is deleted, and with
it the `CodeRabbitReviewer` adapter, its NDJSON parsing, and the
`[review] reviewer` selection (`ReviewerKind`). The `review` verb's wiring into
a reviewer loop (`WorkerActions::review_request`, `with_review_findings`, and
the `findings` step in `build_review_prompt`) is removed, so `jjfx review` is
again one prompt over the PR's review state, failing checks and human comments.

**What was kept.** The tool-enforced review bound. ADR 0016 keeps "the review
bound lives in jjfx" as a decision separate from the one-forge-path cut, and
ticket 04 (resolved, same batch) places it in `ReviewLoop`. The bound and the
generic types it operates on (`Reviewer`, `ReviewRequest`, `ReviewFinding`,
`ReviewOutcome`, `ReviewStop`, `ReviewEscalation`, `ReviewLoopSummary`) survive
in `src/engine/reviewer.rs`; only the bot adapter and the config that selected
it went. `[review] max_passes` remains as the bound's cap. The dispatch
dependency-branch context (a Dispatch feature, not the forge's PR assembly) is
also untouched.

**The prompt.** The delivery commands are gone from the dispatch prompt: no
`gh -R ... pr create`, no `jj git push`, no `Pull Request command` field on
`DeliveryContract`. Delivery is now described as what jjfx does - "jjfx
delivers the change to owner/repo: it rebases your bookmark onto trunk, runs the
repository's check, pushes it, and opens the Pull Request. Do not push or open
the Pull Request yourself." - and the commit description is named as the PR
body. An agent that never reads a forge doc lands its change correctly by
describing it and leaving delivery to jjfx.

**No forge.** A repository with no forge is now clean, not degraded: with
`pull_requests = false`, or with no `origin` remote, the Pull Request step is
`NotApplicable` (which `clean_success` counts as clean) instead of a skip that
retained a reason. A forge that *failed* - an unreachable `gh`, a failed
lookup - is still a skip naming the reason. Two tests prove the no-forge path:
`a_repository_with_no_forge_forges_cleanly` and
`a_repository_with_no_origin_remote_forges_cleanly`, both asserting a clean
`Finished` with no retained progress.

**Verified.** `mise run check` is green (changelog validation, fmt, clippy with
warnings as errors, build, full test suite). The forge suite was rewritten
around the flat path (`pull_request_step_opens_one_flat_pr_per_change`,
`a_change_that_already_has_a_pull_request_is_left_alone`) and the two no-forge
tests added.

Recorded with `momo break` twice: the forge's one-path change, and the removal
of `[review] reviewer`. The prompt change is internal and takes nothing.

