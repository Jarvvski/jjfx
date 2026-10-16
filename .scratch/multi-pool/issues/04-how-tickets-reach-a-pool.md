# How does a Ticket reach a pool?

Type: grilling
Status: ready-for-human
Blocked by: none

## Question

Naming pools is only useful if work can be routed into them, and per-pool Ticket sources make the routing subtle.

- **`--all` becomes pool-scoped.** "All ready Tickets" means "all ready in *this* pool's source". Confirm.
- **An explicit id is ambiguous.** `jjfx dispatch <TICKET>` has no pool to resolve against when two pools share a source. Is `--pool` mandatory for an explicit id? Or resolved by which pool's source can answer for it — and refused loudly as ambiguous when more than one can?
- **Is there label-based routing?** A Ticket's label names its pool, so `--all` fans out across pools. Note the constraint: the local source emits a single synthetic ready label, so label routing needs the local convention extended first. In scope, or explicitly deferred?

Decide the primitive and the convenience separately, and say what an ambiguous call does: refuse, or pick a documented default. Note that a silent wrong choice here is exactly the class of failure the dispatch-ergonomics batch exists to remove — so "refuse loudly" has the weight of precedent.

## Answer

<!-- recorded on resolution -->
