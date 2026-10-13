# Attention precedence when work is stacked above a change-requested PR

Status: ready-for-human
Type: question
Blocked by: none

## Question

When a Workspace has a **changes-requested** PR with a **newer local or pushed
change stacked above it**, which rule wins: the least-delivered state, or
"Waiting over a change-requested PR needs a nudge"?

Split out of ticket 28 (item D6). The answer changes shipped behaviour, so it
needs an intent decision before any code moves.

## What the code does today

`Attention::derive` (`src/attention.rs:52-76`) returns `NeedsYou` only when
`agent == Waiting && work is PrOpen { verdict: ChangesRequested }` (`:59-63`).
`Dirty` and `Pushed` fall through to `ReadyToForge` (`:71-73`).

`WorkState::overlay` (`src/work.rs:321-368`) returns `Dirty` (`:322-330`) or
`Pushed` (`:332-337`) **before** it ever inspects open PRs (`:339-358`). So when
a newer owned commit sits above the PR, the workspace's single `WorkState` is
`Dirty`/`Pushed` and never `PrOpen`.

**Minimal scenario.** Owned chain (tip first) `[T, B]`: `B` is pushed with an
open changes-requested PR, `T` is a newer non-empty local commit (or a pushed
commit with no PR). `overlay` returns `Dirty`/`Pushed`; `derive(Waiting, Dirty)`
is `ReadyToForge`, not `NeedsYou`. The change request is masked.

## Correction to the original claim

The ticket called the nudge rule "unreachable". It is not: the common case - a
Workspace whose working copy is *empty* on top of the PR - has owned chain
`[empty@, B]`, and the empty commit fails the `!commit.empty` guard
(`src/work.rs:322-325`), so `overlay` reaches the PR loop and returns
`PrOpen { ChangesRequested }` → `NeedsYou`. The rule is masked **only** when a
newer non-empty change is stacked above the PR.

## Docs

- Supporting the nudge rule: `docs/adr/0003-two-orthogonal-lifecycles.md:7-8` -
  "an agent Waiting over a change-requested PR needs a nudge, whereas an agent
  Waiting over a Clean workspace is just an idle husk to delete."
  `.scratch/rebuild/PRD.md:24` - "**needs-you**: agent NeedsAttention, or agent
  Waiting over a changes-requested PR".
- Supporting the status quo: `CONTEXT.md:56-59` - "The least-delivered state …
  A local change makes the workspace Dirty even when a lower change already has
  a PR."; `CONTEXT.md:83-88` - a row "shows the lowest PR carrying the most
  blocking verdict".

No doc addresses the stacked case directly, which is why this is a question.

## Tests today

- `src/attention.rs:96-117` `waiting_over_changes_requested_pr_is_needs_you_not_idle`
  locks the non-stacked `NeedsYou` case.
- `src/work.rs:749-768` `overlay_prefers_local_dirty_change_over_lower_open_pr`
  and `overlay_prefers_pushed_change_over_lower_open_pr` lock the masking - but
  both use an **APPROVED** lower PR, so no test exercises masking of a
  *changes-requested* one. The gap is untested; a fix needs a new test at the
  combined scenario, not a tweak of these.

## Options

1. **Nudge wins.** Change `overlay`/`derive` so a lower open changes-requested
   PR still yields `PrOpen { ChangesRequested }` / `NeedsYou`. Matches ADR 0003;
   contradicts CONTEXT's least-delivered statement; a Workspace with a blocked
   PR resurfaces as needs-you even while you are actively adding to the stack.
2. **Least-delivered wins (status quo).** Document the masking as intended. No
   code change, but ADR 0003's rationale silently becomes false for stacked
   work, so CONTEXT and ADR 0003 need amending.
3. **Split the signal.** Keep `Dirty`/`Pushed` for the row but surface the PR
   verdict separately. Neither doc is simply "right"; costs a new field and its
   plumbing.

## Acceptance Criteria

- [ ] The chosen rule is recorded in `## Comments` (or in the doc it changes).
- [ ] A test pins the stacked scenario that is untested today.
- [ ] If the code changes, `mise run check` is green.
