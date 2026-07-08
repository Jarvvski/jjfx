# Epic B - Triage and actions

Type: epic
Status: tracking
Milestone: v0.1

## Goal

Bring both lifecycle axes to life and organize the surface around Attention
(ADR 0003/0008), plus the interactive workspace mutations. Completing A + B is
the v0.1 walking-skeleton-through-the-spine: live triage of concurrent agents
with create/open/delete.

## Entry preconditions

**Epic A must be complete.** Specifically: `03-skeleton-store` provides the app
and store this epic renders onto; `01-spike-hooks` gates `04`; `02-spike-jj-lib`
gates `05`.

## Execution order

1. **04-agent-lifecycle** (needs 01, 03) and **05-work-lifecycle** (needs 02, 03)
   are independent of each other - work them in parallel or in either order.
2. **07-workspace-actions** (needs 03 only) can be worked any time after Epic A -
   slot it alongside 04/05.
3. **06-attention-triage** (needs 04 and 05) is last: it combines both axes into
   the Attention badge and the grouped/sorted list.

## How to work it

- Read `CONTEXT.md` (esp. the two lifecycles + Attention) and ADR 0002/0003/0004
  /0008 before coding.
- Follow the landing gate in `CLAUDE.md`: one focused change per commit,
  `mise run check` green before landing, then `jj describe` / `jj bookmark set
  main --to @` / `jj new`.
- Set each ticket's `Status:` to `in-progress` / `done` as you go.
- v0.1 is user-visible: bump the version + add a dated `CHANGELOG.md` entry in
  the same commit where the behavior lands.

## Definition of done

- A live, Attention-first triage list of concurrent agents: each workspace shows
  its agent state, work state, and derived badge, and re-sorts by push on change.
- `n` / `enter` / `o` / `d` create, open, focus, and delete workspaces via kitty.
- `mise run check` is green; the flow is demoable end-to-end.
