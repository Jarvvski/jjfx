# Decide the landing rule (auto-merge, merge queue, or human-only)

Status: resolved
Type: decision
Blocked by: none

## Problem

The forge pipeline ends at `open-pr`. **Landing is entirely manual** — jjfx has no
merge step at all — and "review & landing" is one of the owner's two named
frictions. There is no stated jjfx rule for when a Workspace's work becomes
merged.

This is a **product decision**, not an agent-decidable task: it is the
opinionated-product stance of the tool.

## Options (pick one, record in `## Comments`)

- **(a) Auto-land on approved + green.** jjfx merges a PR (or a whole stack) when
  the review verdict is approved and required checks pass. Maximally opinionated;
  needs a strong safety story for stacks.
- **(b) Merge-queue handoff.** jjfx enqueues to a GitHub merge queue (or
  `gh stack merge`) and stops; the queue owns conflict handling and ordering.
- **(c) Stay out.** Agents never merge; the human lands. Keeps the current
  contract and files the ticket `wontfix`.

## Solution (if (a) or (b))

Add an opt-in landing step to the forge pipeline gated on the chosen rule, with a
clear confirmation and a per-repo opt-in.

## Commits

1. Record the decision and its reasoning in `## Comments`.
2. If (a) or (b): implement the landing step behind config, reuse the review
   verdict and CI-check data already available, and default to off.
3. Tests; docs; changelog.

## Acceptance Criteria

- [x] The landing rule is decided and recorded.
- [x] If implemented: landing is opt-in, gated on the stated condition, and
      never merges without it.
- [ ] `mise run check` is green.

## Out of Scope

- The stack mechanics — ticket 09.
- Reviewer automation — ticket 08.

## References

- `README.md` ("Never merge" in the skill rules)
- GitHub merge queue:
  https://docs.github.com/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue

## Comments

- 2026-10-15: Filed from the strategy review. Deliberately `ready-for-human` —
  this is the tool's product stance and the owner must set it.
- 2026-10-15: **Decided (option a) — auto-merge on approved + green.** The owner
  ruled, while starting the batch: agents push, open a PR per ticket, and merge
  once the PR's checks are green, then take the next ticket. For this batch the
  gate is the PR's own CI checks (branch `main` is unprotected: no required
  checks, no rulesets, no required reviews). This is the batch's landing rule;
  the *config-gated, opt-in* implementation of it inside jjfx remains a future
  change and is out of scope for the batch itself.

## Resolution

Reconciled Mon 19 Oct 2026 against the merged code at `9d6bd0ce6e7b`.

This ticket was marked resolved without its acceptance boxes ticked. Every criterion was re-verified by an independent read-only review. The reconciliation recorded no new work; it confirms what shipped.

This is a decision ticket, so its criteria are satisfied by the recorded decision rather than by code. Criterion 1 holds: the owner ruled option (a) - agents push, open a PR per ticket, and merge once the PR's checks are green - and it is recorded in the `## Comments` above. Criterion 2 is conditional ("If implemented"); the config-gated, opt-in landing step inside jjfx was deliberately **not** implemented in this batch (the comment records it as a future change and out of scope), so the criterion holds vacuously - there is no landing step that could merge without its condition.

Not ticked: the `mise run check` criterion. It is a landing-commit property, and this reconciliation deliberately does not run the full suite (the orchestrator runs the gate once at the end).
