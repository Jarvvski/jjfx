# Derive the Ticket branch prefix from one source of truth

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

`resolve_ticket_branch` hardcodes the branch namespace as `adam/`
(`orchestration.rs:634`: `format!("adam/{}-", ticket.as_str().to_ascii_lowercase())`),
while dispatch derives the prefix from `jj config user.name`
(`direct_dispatch.rs:534-540`). For any user whose first name is not `adam`,
`resolve_ticket_branch` fails to find the persisted bookmark, and
`revalidate_branches` (`:588-614`) silently rewrites the dependency branch to
`main`, dropping the stacked base and producing a PR based on `main` that
contains the prerequisite's diff. It only passes today because this repo's
`user.name` is `Adam Jarvis`.

## Fix

Derive the branch prefix once and thread it into both `dispatch_prompt` /
`direct_dispatch` and `resolve_ticket_branch`. Never hardcode a namespace.

## Commits

1. Extract the prefix derivation to one function.
2. Use it in `resolve_ticket_branch` and the dispatch prompt builder.
3. Test: a non-`adam` `user.name` still resolves and revalidates its branch.
4. CHANGELOG entry `(fix)`.

## Acceptance Criteria

- [ ] No hardcoded `adam/` remains in the engine.
- [ ] With a non-`adam` `user.name`, a stacked dependency's base is preserved
      (not rewritten to `main`).
- [ ] `mise run check` is green.

## Out of Scope

- Changing the branch-name format itself.
