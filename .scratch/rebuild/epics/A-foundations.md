# Epic A - Foundations

Type: epic
Status: tracking
Milestone: v0.1

## Goal

De-risk the two decisions that could invalidate downstream work (ADR 0002 hooks,
ADR 0007 jj-lib) and stand up the walking skeleton: a ratatui app that reads the
authoritative workspace store, mirrors to `.jj/ws-cache`, and renders a list.
Everything else builds on this.

## Entry preconditions

None. This is the starting epic - an agent can begin immediately.

## Execution order

The two spikes and the skeleton have no blockers and are independent, so they
can be worked in any order (or in parallel). Resolve the spikes early: their
outcomes gate Epic B.

1. **01-spike-hooks** - may amend ADR 0002. Gates Epic B's `04-agent-lifecycle`.
2. **02-spike-jj-lib** - may amend ADR 0007 (or switch jj reads to CLI `-T`).
   Gates Epic B's `05-work-lifecycle`, Epic C's `09`, Epic D's `11`.
3. **03-skeleton-store** - independent of the spikes. Gates almost everything.

## How to work it

- Read `CONTEXT.md` and the ADRs each ticket names before coding.
- Follow the landing gate in `CLAUDE.md`: one focused change per commit,
  `mise run check` green before you land, then `jj describe` / `jj bookmark set
  main --to @` / `jj new`.
- Set each ticket's `Status:` to `in-progress` when you start it and `done` when
  its acceptance criteria pass.
- If a spike contradicts an ADR, stop and amend the ADR (and any affected ticket)
  before building its dependents. This is the whole point of doing the spikes
  first.

## Definition of done

- Both spike findings are recorded and any affected ADR amended.
- `jjfx` launches in a repo, lists workspaces, reconciles a shell-created
  workspace live, and quits with the terminal intact.
- `mise run check` is green.
