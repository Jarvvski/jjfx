# Add Worker Session actions to jjfx

Status: claimed

## Parent

issues/16-integrate-dispatch-into-jjfx.md

## Problem Statement

jjfx must expose the shared Worker actions without confusing Agent Sessions,
Runs, Workers, and Workspaces or losing asynchronous cleanup outcomes.

## Solution

Add Send and Review editors with visible resumed-versus-fresh Session outcomes,
then add confirmed Reset, Rebase, Open PR, alias, and explicitly defined dismiss
actions through the Workspace Dispatch controller.

## Commits

1. Add Send and Review editors and Session outcomes.
2. Add Reset confirmation and separate Workspace restoration completion.
3. Add Rebase, Open PR, alias, and dismiss actions with typed outcomes.
4. Add focused action, cancellation, error, and lifecycle-preservation tests.

## Acceptance Criteria

- [ ] Send and Review work from the selected Worker.
- [ ] Session resume and fresh fallback reasons are visible.
- [ ] Reset retains and reports its asynchronous restoration outcome.
- [ ] Destructive actions use safe confirmation.
- [ ] Alias and dismiss semantics are explicit and tested.
- [ ] `mise run check` is green.

## Out of Scope

- New persistence schemas
- Dispatch Group model changes
- Shared wsg/jjfx startup

## Blocked by

- issues/21-show-dispatch-progress-and-logs.md
