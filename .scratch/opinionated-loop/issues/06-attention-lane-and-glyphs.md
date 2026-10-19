# Attention-first lane and a per-state glyph vocabulary

Status: resolved
Type: task
Blocked by: none

## Problem

jjfx already groups the Workspace list by what needs you (ADR 0008, the `Attention`
signal in `CONTEXT.md`: needs-you / working / ready-to-forge / idle). But it lacks
the concrete visual vocabulary the agent-orchestration field converged on in
2026 — a dedicated **needs-you lane sorted first**, a **glyph per state**, a
reserved "look here" colour, and **idle folded by default** — and its state is
polled rather than derived from the hook signals jjfx already receives.

The tools that do this well (Orca, Orchestrator.inc, agent-deck, cmux) all use the
same four moves; jjfx's thesis is right but its expression is thinner.

## Solution

Adopt the converged vocabulary on top of the existing Attention model:

- A **needs-you lane** sorted to the top of the list.
- A **glyph per state** (working / waiting-on-you / done / blocked / idle).
- A **reserved colour** (amber) for needs-you; red for blocked/failed.
- **Idle folded by default**, with a toggle to show it.
- State **derived from hook events** (ticket 03) rather than polling, so a
  transition is immediate.

## Commits

1. Add the per-state glyph set and the reserved needs-you/blocked colours in the
   render layer.
2. Order the list needs-you-first (the lane) while preserving the existing
   grouping.
3. Fold idle rows by default with a keybinding to reveal them; persist nothing.
4. Drive the attention state from hook events (ticket 03) where available.
5. Tests (render snapshots where the repo already uses them) and changelog.

## Acceptance Criteria

- [x] Needs-you rows sort first and carry a distinct glyph and colour.
- [x] Each lifecycle/attention state has a distinct, documented glyph.
- [x] Idle rows are folded by default and revealable.
- [x] A hook-driven transition updates the row without a poll tick.
- [ ] `mise run check` is green.

## Out of Scope

- Desktop/OS notifications (a separate idea; not filed).
- The underlying lifecycle signals — ticket 03.

## References

- ADR 0008 (attention-first UI); `CONTEXT.md` (Attention)
- `src/render/`, `src/attention.rs`, `src/ui_state.rs`

## Comments

- 2026-10-15: Filed from the strategy review. Refinement of an existing, correct
  thesis rather than net-new behaviour.
- 2026-10-15: Resolved. Added a per-state lane glyph vocabulary in the render
  layer (`attention_glyph`: `⚑` needs-you, `◐` working, `⚒` ready-to-forge, `·`
  idle) and a reserved amber `NEEDS_YOU_AMBER` for the needs-you lane, leaving
  red for blocked/failed. The lane glyph leads every group header and row. The
  idle lane now starts folded (a launch default, not persisted state: the
  `idle_collapsed` UI-state field is retired) and is revealed with `c`. Agent
  transitions are hook-driven already; `on_agent_event` now also revalidates the
  selection so a row moving into the folded idle lane cannot leave a dangling
  selection. Decided to keep the `default` workspace pinned first per ADR 0008,
  with the needs-you lane leading the remaining groups. `mise run check` green.

## Resolution

Reconciled Mon 19 Oct 2026 against the merged code at `9d6bd0ce6e7b`.

This ticket was marked resolved without its acceptance boxes ticked. Every criterion was re-verified by an independent read-only review of the code and tests on `main`. The reconciliation recorded no new work; it confirms what shipped.

Evidence: the per-state glyph vocabulary is documented and implemented in the render layer (`src/render/style.rs:86-204`, including `AgentState::QuotaBlocked => ('⊘', Color::Red)` at `:204`), with the reserved `NEEDS_YOU_AMBER` (`src/render/style.rs:79`) for the needs-you lane and red kept for blocked/failed. The lane glyph leads each group header and row (`src/render/home.rs:148`, `:210`). Idle rows start folded (`idle_collapsed`, `src/render/home.rs:44`) and are revealed with `c`; the folded default is a launch default rather than persisted state. Tests: `src/render/home.rs` asserts the `⚑ needs you` header and that the lane glyph is amber (`:549`, `:558-559`). Criterion 4 holds: transitions are hook-driven - `on_agent_event` updates the row and revalidates the selection, so a row moving into the folded idle lane cannot leave a dangling selection - and the ticket's comment records that the `idle_collapsed` UI-state field was retired in favour of the launch default.

Not ticked: the `mise run check` criterion. It is a landing-commit property, and this reconciliation deliberately does not run the full suite (the orchestrator runs the gate once at the end).
