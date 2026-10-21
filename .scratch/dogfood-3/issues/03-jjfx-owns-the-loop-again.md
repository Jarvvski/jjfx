# jjfx owns the bounded review loop again, sourced from the PR's review threads

Status: resolved
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

- [x] A PR's unresolved review threads become `ReviewFinding`s, and a PR with none
  converges without sending anything.
- [x] `ReviewLoop` has a caller in the binary, bounded by `[review] max_passes` and by
  the progress rule, and a pass already recorded in the decision log is replayed
  rather than re-sent.
- [x] A loop that stops on the cap or on a stalled round **escalates** its unresolved
  findings for a human - it never reports success.
- [x] `jjfx help review`, `docs/review-loop.md` and `docs/configuration.md`'s
  `[review]` section describe what the command actually does, and a test holds
  the three together.
- [x] A repository with no forge, or a Worker with no Pull Request, is refused by
  name rather than silently reporting convergence.
- [x] `mise run check` passes; a user-visible change is recorded with `momo add`.

## Resolution

The verb shape: `jjfx review <worker>` **drives the bounded loop**. Each pass
reads the Pull Request's unresolved review threads through `gh`, maps each open
thread onto a `ReviewFinding`, and sends the pass's findings to the Worker to
address. The bound is `ReviewLoop`, keyed per pass by
`review:<ticket>:threads:<pass>` and driven through the pool's decision log, so a
recorded pass replays rather than re-reading `gh` or re-sending. Only a pass that
finds nothing is success; any other stop escalates and exits non-zero.

What changed:

- `src/engine/review_threads.rs` (new) - the `ReviewThreads` seam (so a test
  supplies threads with no network), `GhReviewThreads` (the production
  `gh api graphql` read of `reviewThreads`, keeping only `isResolved: false`), and
  `ThreadReviewer<S>` mapping each thread onto a `ReviewFinding` (file, lines,
  title from the first comment, the comments as the body, and any `suggestion`
  fence as a suggested change). Registered in `src/engine/mod.rs`.
- `src/engine/reviewer.rs` - `ReviewRequest` gained an optional Pull Request
  number (`with_number`/`number`); `ReviewLoop` now honours `ReviewCompleteness`,
  so an incomplete pass stops on a new `ReviewStop::Incomplete` and escalates
  (with the reason) instead of converging; `ReviewEscalation` and
  `ReviewLoopSummary` carry that reason.
- `src/engine/worker_actions.rs` - `review_target` (resolve the PR once and refuse
  a missing forge or PR by name), `address_review` (the address step: the review
  prompt carrying the target PR, its failing checks and the pass's findings), and
  `worker_ticket`. `review` is now `address_review` with no findings, so the TUI
  `v` key is unchanged. `build_review_prompt` lists a pass's findings when it has
  them and keeps the fetch-comments step when it does not. `PullRequest` carries
  `baseRefName`.
- `src/cli.rs` - `review_command` is the loop's caller.
- `tests/review_contract.rs` (new) - holds `jjfx help review`,
  `docs/review-loop.md` and `docs/configuration.md`'s `[review]` section together,
  and drives the loop end to end with fake `jj`/`gh`/`claude`. Hermetic: `JJ_CONFIG`
  dropped, `XDG_CONFIG_HOME` an empty temp dir, and a repo-local jj identity.
- Docs: `docs/review-loop.md`, `docs/configuration.md`, `src/cli_help.rs`,
  `src/config.rs`, `README.md`, `skills/jjfx-dispatch/SKILL.md` and the playbook.

### The three questions

1. **What progress means when the findings are PR threads; does addressing a
   thread resolve it?** Addressing a thread does **not** resolve it. Resolution is
   the reviewer's action - a human or a bot resolves the conversation - and neither
   jjfx nor the Worker does it. So a pass that sends findings leaves its threads
   open; if the next pass sees the same open threads and retires none, the
   progress rule reads a stall and the loop stops on `NoProgress`, reported as
   such rather than as a pass. This follows ADR 0016's "verified done": the
   Worker's self-report is not verification, so the tool does not close a thread
   on the Worker's say-so.
2. **What "unresolved" means on a PR whose review is a bot's comment rather than
   a thread.** A plain issue comment is **not a finding**. Only a review thread
   carries an `isResolved` state; a comment has none, so it could never be
   retired and would make the loop unable to converge. The findings source is
   unresolved review threads alone; the address prompt still points the Worker at
   the PR's comments, but the bound counts threads.
3. **What the loop does with a PR that has no review at all.** It **converges on
   the first pass**: no threads is no findings, which is exactly the empty pass
   the loop stops on. Stated explicitly in the module docs and in
   `docs/review-loop.md`, not left to fall out of the mapping.

A read that failed is **not** "no findings": `ThreadReviewer` returns an
incomplete outcome, and the loop stops on `ReviewStop::Incomplete` and escalates
rather than reading an unreadable review as clean.

Out of scope, deliberately: the TUI's `v` key is still a single address prompt.
The three surfaces the ticket names are the CLI's, and wiring the bounded loop
into the TUI's async dispatch is a separate change.

