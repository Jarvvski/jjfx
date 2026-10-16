# Do the maintenance verbs need a pool scope?

Type: grilling
Status: ready-for-human
Blocked by: none

## Question

`Lift`, `Tidy workspaces` and `Clean` are defined per Workspace or per Repository. With several pools, is a pool a scope any of them needs?

- **Clean** removes every non-default Workspace in one sweep. Does a Repository-wide sweep still make sense when the Workspaces belong to different lanes — or does each pool want its own, so clearing "bugs" leaves "features" running?
- **Tidy workspaces** moves every idle Workspace onto the latest trunk. Pool-relative or Repository-relative?
- **Lift** rebases one Workspace's chain, or all of them. Same question.
- **Reset** and **Dismiss** are per-Worker and per-pool by construction, so they likely need nothing — confirm rather than assume.

The likely answer is "Repository-scoped still, because a Workspace is a Workspace regardless of which pool it belongs to" — but **Clean** is genuinely arguable, and getting it wrong means one lane's tidy-up disturbs another lane's work in progress.

## Answer

<!-- recorded on resolution -->
