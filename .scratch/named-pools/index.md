# Named Worker Pools - implementation

## What this is

The implementation backlog for **running several named Worker Pools in one Repository**, cut from the design that `.scratch/multi-pool/` locked on 2026-10-16. That effort decided everything; read its `map.md` for the decisions, and these three ADRs for the reasoning that constrains the work:

- `docs/adr/0012-named-worker-pools.md` - a Repository holds one or more Worker Pools, and a Worker belongs to exactly one for its life.
- `docs/adr/0013-persist-pool-state-per-pool.md` - a directory per pool; pool-scoped state under the pool and repository-scoped state at the root; locks both, repository outermost; an automatic one-time conversion.
- `docs/adr/0014-repository-local-pool-declarations.md` - a repository-local pool declaration may **select, not grant**.

**Standing stance: additive, not a redesign.** `jjfx pool <N>` keeps sizing `default`. `status --json` keeps its flat `workers[]` and `waiting[]`, each gaining a `pool` field, with per-pool spend arriving as a *sibling* `pools` key - never by nesting. Worker addressing is unchanged, because names are already globally unique. The only non-backward-compatible thing is the persisted layout, and it is not a public surface: the `wsg` contract that would have made it one is dead, and its residue is already retired.

## Tickets

| # | Ticket | Blocked by |
| --- | --- | --- |
| 01 | Declare pools in `[pools]` | - |
| 02 | Persist a directory per pool, converting an existing one | - |
| 03 | Locks: per-pool inside a repository-wide lock | 02 |
| 04 | Pool lifecycle: create, destroy, and size `default` | 01, 02 |
| 05 | Removing a pool drains, and reports what it discards | 04 |
| 06 | Dispatch targets a pool | 01, 02 |
| 07 | Route a Ticket to its pool by label | 06 |
| 08 | Per-pool spend under the global ceiling | 06 |
| 09 | `status --json` reports pools | 06 |
| 10 | `clean --pool` | 02 |
| 11 | Sweep the singular "the Pool" from the codebase | all |
| 12 | A repository-local `[tickets]` source selection | 01 |
| 13 | A declared pool's state is never materialised | - |
| 14 | The per-pool `ready_status` is declared but unused | 07 |

## Suggested order

**01 and 02 are independent roots** and can run in parallel - which makes them the two worth dispatching first. Everything else hangs off one or both.

**11 is deliberately last.** It is a mechanical sweep of every bare "the Pool" across comments, error strings and docs, and running it last keeps it from conflicting with every ticket above it. ADR 0012 already notes it touches the state layer, the CLI and ADR 0011.

**12 is a follow-on, not a prerequisite.** Nothing here needs it; it exists because ADR 0014's capability rule makes it possible, and it is the wall the `todo-md` test-drive hit.

**13 and 14 were filed while implementing the rest**, and both are gaps the effort left rather than new scope. **13 is the last mile**: nothing materialises a declared pool's state, so a named lane can be declared and never used - every other ticket is plumbing until it lands. 14 is the smaller half of the same honesty rule: a configuration key that parses and does nothing.

## Out of scope

- **The TUI** - deferred by the design effort to its own work once the model settles.
- **Multi-repository pools** - a Worker Pool is held by one Repository.
- **Moving or borrowing Workers between pools** - membership is the model, so an idle "features" Worker cannot take a "bugs" Ticket.
