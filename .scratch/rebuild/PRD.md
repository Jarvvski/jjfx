# jjfx rebuild - PRD

## Core idea

A workspace is the unit of parallel agent work. jjfx is keyboard-driven
mission-control for many concurrent Claude Code agents, each isolated in its own
Jujutsu workspace, shepherding each workspace from creation to merge.

This is a re-conception of the existing `jj-wsx` Bun/Ink TUI around a sharper
core - not a faithful port. Feature parity is explicitly not a goal.

## Model

See `CONTEXT.md` for the glossary. Each workspace carries two orthogonal
lifecycles:

- **agent**: Absent -> Working -> Waiting -> NeedsAttention -> Ended
  (event-sourced from Claude Code hooks)
- **work**: Clean -> Dirty -> Pushed -> PrOpen -> Merged (from jj-lib + gh)

The list surfaces one derived **Attention** badge per workspace. Derivation
(tunable):

- **needs-you**: agent NeedsAttention, or agent Waiting over a changes-requested PR
- **working**: agent Working
- **ready-to-forge**: Dirty/Pushed work sitting idle (agent not Working)
- **idle**: Clean work and no live agent

## Decisions

See `docs/adr/`:

- 0001 observe, don't orchestrate (yet)
- 0002 event-source the agent lifecycle from Claude Code hooks
- 0003 two orthogonal lifecycles
- 0004 hook -> TUI transport = append-only global JSONL log; dumb hooks, TUI folds
- 0005 self-contained native reimplementation, coexisting with the bash tools
- 0006 own a rich per-repo store; `.jj/ws-cache` is a lossy mirror
- 0007 data access: jj-lib + syntect + `gh --json`
- 0008 attention-first UI

## Seams and engine

- kitty behind a `Terminal` trait (v1 kitty-only); IDE deferred behind an
  `Editor` seam.
- Identity derive-first (gh login / `trunk()` / ws convention), optional
  `jjfx.toml` override.
- Engine: ratatui + crossterm + tokio; background tasks -> channel -> one owned
  `App` state; jj-lib on `spawn_blocking`; restore-on-panic.

## Epics and milestones

| Epic | Milestone |
| --- | --- |
| A - Foundations | v0.1 |
| B - Triage and actions | v0.1 |
| C - Forge and maintenance | v0.2 |
| D - Detail views | v0.3 |

v0.1 = A + B. v0.2 = C. v0.3 = D.
