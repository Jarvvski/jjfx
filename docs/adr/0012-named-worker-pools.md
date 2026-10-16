# Give a Repository several named Worker Pools

Status: accepted

## Context

ADR 0009 adopted Workspace Dispatch with **Worker Pool** as "the Repository-scoped collection of reusable Workers and their execution capacity" — one pool per Repository — and `Repository` was defined as "the scope of its Worker Pool". That made a Repository's execution capacity a single number: one size, one Agent Runtime Profile, one spend budget, one Ticket source.

That is too coarse for the work jjfx is used for. Bugs and features want different concurrency, different models, and separate budgets — and a Repository cannot currently declare any of it, because `.jjfx/config.toml` accepts only `[checks]`.

## Decision

A Repository holds **one or more Worker Pools**. A Worker Pool is one named collection of reusable Workers and their execution capacity, owning:

- its **size** — the number of Workers it holds;
- its **Agent Runtime Profile** — the provider and model its Runs launch with;
- its **spend cap**, under a Repository-wide ceiling that still binds; and
- its own **Ticket source**.

A Worker **belongs to exactly one Worker Pool for its life**. Membership is the pool's `workers` list, as it already is. A Worker is not assigned to a pool per Dispatch, because a Worker is backed by exactly one Worker Workspace and that Workspace accumulates one Change chain — a Worker that moved between lanes would carry one lane's work into another.

`Worker Pool` stays the term for the unit, and the collection is given no new noun: a Repository holds *its Worker Pools*. Consequently the bare definite "the Pool" is no longer meaningful on its own and must say which pool it means.

`default` is the pool every Repository starts with, so `jjfx pool <N>` keeps its meaning and an existing Repository migrates by naming its single pool.

## Considered options

- **Assignment per Dispatch** — pool-agnostic Workers claimed per Dispatch. Rejected: it mixes one lane's Change chain into another lane's Worker Workspace, and makes "size" meaningless.
- **A collection noun** ("Pool Set") — rejected as a term almost nothing operates on; the phrase "the Repository's Worker Pools" carries it.
- **Renaming the unit to "Lane"** — rejected: a large vocabulary break across `jjfx pool`, the state files, ADR 0009 and the migration PRD, for a naming win.

## Consequences

- Every bare "the Pool" in code comments, error strings and documentation becomes ambiguous and must be made explicit. Mechanical, not a design change — but it touches the state layer, the CLI and ADR 0011.
- **Dispatch Eligibility** is now relative to the pool being dispatched into.
- Capacity is no longer a Repository-wide number, so anything that reported or capped it — spend, `pool list`, `status` — must say which pool it means and what the Repository-wide total is.
- The persisted shape, the CLI and config surface, and the routing rules are decided by the tickets under `.scratch/multi-pool/`, not here.
