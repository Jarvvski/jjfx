# Ingest an AI reviewer's findings into the review loop

Status: resolved
Type: task
Blocked by: none

## Problem

`review` already does more than it looks: `build_review_prompt`
(`src/engine/worker_actions.rs:1052`) reads the PR's review state, **inlines
failing CI checks**, and tells the Worker to fetch review comments and inline
threads, then address them. But the loop is **one-shot and human-only** — nothing
*runs* a reviewer, so the owner is the reviewer for every PR, and there is no
bounded re-review cycle.

## Solution

Add a configured AI reviewer that produces findings, normalise them, and route
them through the existing `review` verb to the Worker's authoring agent, with a
bounded number of re-review iterations.

- First implementation: **CodeRabbit CLI** in agent mode — `cr --agent` emits
  NDJSON, has structured exit codes, and supports headless auth
  (`cr auth login --api-key`) and `cr review --remote owner/repo` with no local
  checkout.
- Alternative: the **opencode GitHub Action** (the owner's runtime) as a
  repo-side reviewer.

## Commits

1. Add a `[review]` config section (reviewer = none | coderabbit | opencode-action)
   and an adapter that invokes the reviewer and returns normalised findings.
2. Feed normalised findings into the existing review prompt alongside the human
   comments and CI checks.
3. Add a bounded re-review loop (e.g. review -> address -> review N times), with
   a clear stop condition and cost awareness.
4. Tests against a fake reviewer; docs; changelog.

## Acceptance Criteria

- [x] With a reviewer configured, `jjfx review <worker>` fetches the reviewer's
      findings and inlines them into the Worker's prompt.
- [x] The re-review loop is bounded and stops on a clear condition.
- [x] With no reviewer configured, behaviour is exactly today's.
- [x] Reviewer auth is headless-capable and never leaks credentials into argv.
- [x] `mise run check` is green.

## Out of Scope

- Reviewer vendor choice beyond the first adapter.
- Auto-landing — ticket 10.

## References

- CodeRabbit CLI: https://docs.coderabbit.ai/cli
- opencode GitHub action: https://opencode.ai/docs/github/
- `src/engine/worker_actions.rs:523,1052`

## Comments

- 2026-10-15: Filed from the strategy review. Corrects an earlier assumption: CI
  checks and comments are *already* ingested — this ticket automates the loop.
- 2026-10-15: The bounded-loop item now has measured evidence, from a 24-ticket
  batch run through parallel agents. Of 901 agent sessions, **642 (65% of the
  batch's ~$174 cost) were the review fan-out** — seven dimensions per pass, plus
  a scoring child per finding. One work stream reached **Pass 18**: 144 child
  sessions costing $17.61 of review around $1.49 of work, re-running all seven
  dimensions each round and finding nothing new after the second pass. The loop
  was instructed to run "to zero" and treated zero as reachable by repetition, so
  it terminated on a session limit rather than on convergence. Two things follow
  for this ticket: the stop condition needs to be a **pass cap**, not a
  findings-count; and the marginal dimension's value collapses after pass 2, so
  a re-pass should re-run only the dimensions that found something.
- 2026-10-15: Resolved. `[review] reviewer` selects the reviewer (`none` is the
  default, and `none` runs neither a reviewer nor a loop, so today's prompt is
  byte-for-byte unchanged); `[review] max_passes` is the pass cap. The first
  adapter is the CodeRabbit CLI in agent mode (`cr review --agent --remote`),
  which the tests drive through a fake `cr` on disk: findings normalise from the
  NDJSON stream, an incomplete run keeps the findings it did emit, and the argv
  is asserted exactly so no credential can ever be on it (`cr` reads its own
  stored auth, so headless login stays out of band). `ReviewLoop` bounds the
  cycle by the cap and stops early only when a pass has nothing to address; it
  reports passes, findings, and why it stopped. A background Run cannot wait for
  an address pass, so `--fg` runs the cap and a background review is one pass.
  Deviations worth noting: the `ReviewLoop` type landed in the same commit as
  the config and adapter (same module) rather than in the loop commit, which
  only wires it; and `opencode-action` parses but is a load-time setup error, as
  the ticket's Out of Scope allows.
