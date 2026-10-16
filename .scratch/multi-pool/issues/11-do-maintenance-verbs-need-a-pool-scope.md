# Do the maintenance verbs need a pool scope?

Type: grilling
Status: resolved
Blocked by: none

## Question

`Lift`, `Tidy workspaces` and `Clean` are defined per Workspace or per Repository. With several pools, is a pool a scope any of them needs?

- **Clean** removes every non-default Workspace in one sweep. Does a Repository-wide sweep still make sense when the Workspaces belong to different lanes — or does each pool want its own, so clearing "bugs" leaves "features" running?
- **Tidy workspaces** moves every idle Workspace onto the latest trunk. Pool-relative or Repository-relative?
- **Lift** rebases one Workspace's chain, or all of them. Same question.
- **Reset** and **Dismiss** are per-Worker and per-pool by construction, so they likely need nothing — confirm rather than assume.

The likely answer is "Repository-scoped still, because a Workspace is a Workspace regardless of which pool it belongs to" — but **Clean** is genuinely arguable, and getting it wrong means one lane's tidy-up disturbs another lane's work in progress.

## Answer

Resolved 2026-10-16. One principle decides all four: **a verb is pool-scoped when its blast radius is Workers, and Repository-scoped when it is trunk.**

- **`Clean` gains an optional pool scope.** It removes every non-default Workspace in one sweep, which in a Repository with lanes means one lane's tidy-up destroys another lane's work in progress. That is the one genuine case here: `clean --pool <name>` scopes it, the Repository-wide form stays as the explicit "everything", and the default stays Repository-wide so nothing existing changes (*Is this additive, or a redesign?*).
- **`Tidy workspaces` stays Repository-scoped.** It moves every *idle* Workspace onto the latest trunk, and trunk is shared — which pool a Worker belongs to does not move trunk. A pool scope would add a filter and no safety, because idle is already the guard.
- **`Lift` stays Repository-scoped.** It rebases one Workspace's chain, or all of them; a chain is a chain and its base is trunk.
- **`Reset` and `Dismiss` need nothing** — confirmed rather than assumed. `Reset` is per-Worker and `Dismiss` is per-pool by construction, so both already operate below the granularity a pool scope would add.

The ticket's guess holds for three of the four and fails for `Clean`, which is the only one that can disturb a neighbour.
