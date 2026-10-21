# jjfx owns the bounded review loop again, sourced from the PR's review threads

Status: ready-for-agent
Type: task
Blocked by: none

## Why

`dogfood-2`'s finding 04 recorded the gap: `jjfx review <worker>` sends one prompt
and produces no findings, so `ReviewLoop` - the tool-enforced bound ADR 0016 kept
in jjfx - has no caller in the binary at all. Its only caller is the harness seam
(`RecordedReviewLoop`). The second drive's report left the boundary open:
*jjfx is not a reviewer*, or *the reduction needs jjfx to own the loop after all*.

**The owner has ruled (2026-10-21): jjfx owns the loop, and the findings are the
Pull Request's review threads, read through `gh`.** Not an external review bot's
ingestion - that stays cut by ADR 0016 - and not a jjfx-dispatched reviewer agent.
The *bound* stays in jjfx; only the findings source is named.

## What to build

- **A `Reviewer` over the PR's review threads.** `src/engine/reviewer.rs` already
  defines `trait Reviewer { fn review(&self, request: &ReviewRequest) ->
  Result<ReviewOutcome, ReviewerError> }` and `ReviewFinding` carries a severity,
  a file and lines, a title, a body and suggestions. The new implementation reads
  the Pull Request's **unresolved review threads** through `gh` and maps each
  thread onto a `ReviewFinding`; a PR with no unresolved threads is a
  `ReviewOutcome` with no findings, which is what lets the loop converge.
  `ReviewCompleteness` exists for the case where the threads cannot be read at
  all - a failure to read is not "no findings".
- **A caller that drives the loop.** `ReviewLoop::run(reviewer, request, address)`
  takes the address step as a closure, and `ReviewLoop::drive` keys each pass by
  its step identity so a recorded pass is replayed rather than re-sent
  (`ReplayingReviewer` over the pool's decision log, `.jj/pools/<name>/decisions.json`).
  The address step sends the pass's findings to the Worker - the existing review
  prompt is the natural place, and `WorkerActions::review` already builds and
  sends one - so a pass that finds nothing never sends anything.
- **The verb's meaning.** `jjfx review <worker>` currently sends one prompt and
  claims nothing more; `docs/review-loop.md` and `configuration.md`'s `[review]`
  section both say so, and `jjfx help review` must agree with whatever the
  command now does. Whatever shape is chosen, the three surfaces must not
  disagree - that was the whole of `dogfood-2`'s finding 04.

## The questions the implementer must answer, not assume

- **What progress means when the findings are PR threads.** A thread is resolved
  by a human or a bot, not by the Worker. If a pass sends findings and the
  threads are still open next pass, the loop's progress rule sees a stall and
  stops on `NoProgress` - which is honest, and must be reported as such rather
  than as a review that passed. Decide whether addressing a thread *resolves* it,
  and say which.
- **What "unresolved" means on a PR whose review is a bot's comment rather than a
  thread.** A plain issue comment has no resolution state; decide whether it is a
  finding and document the decision.
- **What the loop does with a PR that has no review at all.** Converged on the
  first pass is the likely answer; make it explicit rather than incidental.

## Acceptance Criteria

- A PR's unresolved review threads become `ReviewFinding`s, and a PR with none
  converges without sending anything.
- `ReviewLoop` has a caller in the binary, bounded by `[review] max_passes` and by
  the progress rule, and a pass already recorded in the decision log is replayed
  rather than re-sent.
- A loop that stops on the cap or on a stalled round **escalates** its unresolved
  findings for a human - it never reports success.
- `jjfx help review`, `docs/review-loop.md` and `docs/configuration.md`'s
  `[review]` section describe what the command actually does, and a test holds
  the three together.
- A repository with no forge, or a Worker with no Pull Request, is refused by
  name rather than silently reporting convergence.
- `mise run check` passes; a user-visible change is recorded with `momo add`.
