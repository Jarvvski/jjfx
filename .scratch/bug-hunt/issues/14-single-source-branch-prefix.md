# Derive the Ticket branch prefix from one source of truth

Status: resolved
Type: task
Blocked by: none

## Problem

`resolve_ticket_branch` hardcoded the branch namespace as `adam/` while dispatch
derives the prefix from `jj config user.name`. For any operator whose first name
is not `adam`, `resolve_ticket_branch` missed the persisted bookmark and
`revalidate_branches` silently rewrote the dependency branch to `main`, dropping
the stacked base and producing a PR based on `main` that contained the
prerequisite's diff. It only passed because this repo's `user.name` is
`Adam Jarvis`.

## Fix

Match on the **Ticket**, not a namespace: a bookmark whose final component is
`<ticket>` or `<ticket>-<slug>` (case-insensitive). This removes the hardcoded
prefix entirely and is also robust to `user.name` changing between dispatch and
resume.

## Commits

1. `Resolve the Ticket branch by Ticket id, not a namespace` - the resolution
   change, the existing repair test switched to a non-`adam` prefix so it fails
   on the old code, and a CHANGELOG `(fix)`.

## Acceptance Criteria

- [x] No hardcoded `adam/` remains in the engine.
- [x] With a non-`adam` `user.name`/branch prefix, a stacked dependency's base
      is preserved (not rewritten to `main`).
- [x] `mise run check` is green.

## Out of Scope

- Changing the branch-name format itself.
