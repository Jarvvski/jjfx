# Steps keyed by identity, so a re-run does not duplicate work

Status: resolved
Type: task
Blocked by: none

## Problem

The same work is done repeatedly and unknowingly. The seven review dimensions
recur **8-11x** as duplicate titles across rounds, and 54 sessions are
`(fork #N)` re-forks of long conversations - one title 33 times.

Nothing keys a step to an identity, so nothing can tell "this has already been
done" from "this needs doing". The research's pattern is to make
side-effecting steps idempotent and replay recorded decisions, so a retry is
safe and a repeat is detectable.

## Acceptance Criteria

- [x] A step has an identity derived from what it is (ticket, dimension, pass),
      recorded durably.
- [x] Re-running a completed step is detectable and reported, not silently
      repeated.
- [x] Replay of a recorded decision does not re-invoke the model.
- [x] A test drives the same step twice and asserts the second is recognised.

## References

- `.scratch/harness/issues/06-durable-queue.md`
- `src/engine/state.rs` (`RunRecord`)
- `docs/work-queue.md`

## Resolution

Built on `06`'s queue rather than beside it.

**Identity (criterion 1).** `StepKey::review(ticket, dimension, pass)` composes
`review:<ticket>:<dimension>:<pass>`, and `QueuedStep::review` records it in the
durable queue with kind `review` and a subject naming all three. The review kind
is the recurring one: the same dimension on a later round, and a re-pass over
every dimension, are the same steps, while two passes or two dimensions are
distinct. A Dispatch step and a review step for one Ticket are different work.

**Detection (criterion 2).** `06`'s idempotent `enqueue` already returns
`QueueAdmission::Known { state }` for a step already recorded, including a
completed one; the review step now exercises it. Offering a settled review step
again is reported `Known { Done }` and adds no second item, and the completed
item stays readable through `jjfx status --json`.

**Replay (criterion 3).** A step's *decision* - for a review pass, its
`ReviewOutcome` - is kept in a second sidecar,
`.jj/pools/<name>/decisions.json`, keyed by the same step identity and written
with the state layer's atomic replace under its own lock. `ReplayingReviewer`
reads a recorded decision and returns it without calling the reviewer behind it;
only a step the log does not know reaches the model, and its fresh decision is
recorded for the next repeat to replay. This is a mechanism with tests, not yet
a command: nothing in the binary drives a review pass through it, because the
review loop still stands on its own until the harness drives it
(`src/engine/reviewer.rs`). Replay therefore reads a stored `ReviewOutcome`
(findings and completeness) from disk and returns it unchanged.

**Test (criterion 4).** `tests/idempotent_steps.rs` drives the same review step
twice through a fresh handle and asserts the second is `Known { Done }` with one
item; it also records a decision, reopens the Repository, and replays it through
a reviewer that panics if it is asked. The queue's unit tests cover the review
identity and the repeat, and the decision store's cover durability and
idempotent recording.

Verified by `mise run check` (changelog validation + fmt + lint + build + test).

**Changelog:** none. Nothing an operator can see or configure changed: no command
emits a review step yet, the decision sidecar is internal, and `status --json`
is unchanged for existing work. This is internal machinery.
