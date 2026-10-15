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

- [ ] Needs-you rows sort first and carry a distinct glyph and colour.
- [ ] Each lifecycle/attention state has a distinct, documented glyph.
- [ ] Idle rows are folded by default and revealable.
- [ ] A hook-driven transition updates the row without a poll tick.
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
