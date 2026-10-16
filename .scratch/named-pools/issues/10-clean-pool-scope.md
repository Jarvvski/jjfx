# `clean --pool`

Type: task
Status: resolved
Blocked by: 02

## Problem

`Clean` removes every non-default Workspace in the Repository in one sweep. In a Repository with lanes that means one lane's tidy-up destroys another lane's work in progress - the one maintenance verb whose blast radius is Workers rather than trunk.

*Do the maintenance verbs need a pool scope?* settled all four with one principle - **pool-scoped when the blast radius is Workers, Repository-scoped when it is trunk**:

- **`Clean` gains an optional pool scope.** `clean --pool <name>` scopes it; the Repository-wide form stays as the explicit "everything"; and the **default stays Repository-wide** so nothing existing changes.
- **`Tidy workspaces` stays Repository-scoped** - trunk is shared, and idle is already the guard.
- **`Lift` stays Repository-scoped** - a chain is a chain and its base is trunk.
- **`Reset` and `Dismiss` need nothing** - they already operate below the granularity a pool scope would add.

This ticket is only the `Clean` half; the other three were confirmed as needing no change, so if you find yourself editing them, stop and say why.

## Acceptance Criteria

- [x] `clean --pool <name>` removes only that pool's non-default Workspaces.
- [x] The default stays Repository-wide, so existing invocations are unchanged.
- [x] An unknown pool name is a loud error naming the pools that exist.
- [x] `Tidy`, `Lift`, `Reset` and `Dismiss` are untouched.
- [x] `mise run check` is green.

## References

- `src/cli.rs` (the clean command), `src/engine/workspace.rs` (`WorkspaceCleanPlan`)
- `.scratch/multi-pool/issues/11-do-maintenance-verbs-need-a-pool-scope.md`
