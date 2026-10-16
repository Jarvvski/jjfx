# Route a Ticket to its pool by label

Type: task
Status: resolved
Blocked by: 06

## Problem

Requiring `--pool` on every Dispatch makes lanes capacity buckets that a human has to route by hand. The point of named pools is that work lands in the right lane by itself.

*How does a Ticket reach a pool?* settled the convenience:

- **A pool declares the label that routes to it** (`[pools.bugs] label = "bug"`), and `dispatch --all` fans out across pools, sending each Ticket to the pool whose label it carries.
- A pool declaring its own label beats a magic `pool:<name>` string, because a lane can adopt a vocabulary you already use.
- A Ticket matching **no** pool is **reported, not dropped** - the same principle as the ambiguity refusal in ticket 06.
- The ready label is per pool, defaulting to the global value.

**Prerequisite:** the local ticket convention needs a `Labels:` metadata line. Its parser already ignores unknown keys, so the line is inert until the source reads it; the Linear source already exposes labels.

## Scope

- The local markdown convention gains a `Labels:` line, parsed into the Ticket's labels.
- `dispatch --all` fans out across pools and routes each Ticket by its label.
- A Ticket matching no pool's label is reported with the reason.
- An explicit `--pool` still wins, and still means "only this lane".

## Acceptance Criteria

- [x] The local convention parses `Labels:`, and an existing ticket without the line is unaffected.
- [x] `dispatch --all` routes each Ticket to the pool whose label it carries.
- [x] A Ticket matching no pool is reported, not silently skipped.
- [x] An explicit `--pool` overrides label routing.
- [x] `mise run check` is green.

## References

- `src/engine/local_tickets.rs` (`LocalTicket::parse`), `src/engine/ticket.rs` (labels)
- `docs/local-tickets.md`
- `.scratch/multi-pool/issues/04-how-tickets-reach-a-pool.md`
