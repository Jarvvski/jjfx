# How does a Ticket reach a pool?

Type: grilling
Status: resolved
Blocked by: none

## Question

Naming pools is only useful if work can be routed into them, and per-pool Ticket sources make the routing subtle.

- **`--all` becomes pool-scoped.** "All ready Tickets" means "all ready in *this* pool's source". Confirm.
- **An explicit id is ambiguous.** `jjfx dispatch <TICKET>` has no pool to resolve against when two pools share a source. Is `--pool` mandatory for an explicit id? Or resolved by which pool's source can answer for it — and refused loudly as ambiguous when more than one can?
- **Is there label-based routing?** A Ticket's label names its pool, so `--all` fans out across pools. Note the constraint: the local source emits a single synthetic ready label, so label routing needs the local convention extended first. In scope, or explicitly deferred?

Decide the primitive and the convenience separately, and say what an ambiguous call does: refuse, or pick a documented default. Note that a silent wrong choice here is exactly the class of failure the dispatch-ergonomics batch exists to remove — so "refuse loudly" has the weight of precedent.

## Answer

Resolved with the owner, 2026-10-16. The primitive and the convenience are separate decisions, and they landed differently.

**Primitive — one pool implies it; several require `--pool`.** The rule keys off the number of pools, not off sources. With one pool — every existing Repository — nothing changes: `jjfx dispatch AMBA-42` works exactly as today, which is the additive stance. With several, an explicit id must name its pool. Resolving by *which source knows the id* was rejected as the primitive because it cannot disambiguate two pools on the same tracker — the common case, two Linear pools split by label — and because it costs a discovery query per pool on every dispatch. It stays available later as a convenience.

`--text` and `--from-file` follow the same rule: `default` when there is one pool, `--pool` required when there are several.

**Convenience — label routing is in the design.** A pool declares the label that routes to it (`[pools.bugs] label = "bug"`), and `dispatch --all` fans out across pools, sending each Ticket to the pool whose label it carries. A pool declaring its own label beats a magic `pool:<name>` string: it lets a lane adopt an existing vocabulary ("bug", "ui") rather than requiring a new one. A Ticket matching no pool is reported, not dropped.

Stated prerequisite: the local convention needs a `Labels:` metadata line. Its parser already ignores unknown keys, so the line is inert until the source reads it, and Linear already exposes labels. That is an implementation follow-on rather than a map ticket — this map's destination is the design.

**Consequences pushed into ticket 05** (the config surface), recorded there:

- The ready label is naturally **per pool** — each pool's source may discover by a different label — so `--label` becomes an override on the pool's configured value, not a global default.
- Whether a pool may have **no Ticket source** (a lane reachable only by an explicit `--pool`) is a schema question.
- **Duplicate routing labels** must be refused at config load, not resolved by precedence.

No ADR: every decision here is reversible and lives on the CLI/config surface, which is the opposite of the persisted layout this map decided last.
