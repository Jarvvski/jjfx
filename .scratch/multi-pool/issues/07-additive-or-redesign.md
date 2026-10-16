# Is this additive, or a redesign?

Type: grilling
Status: resolved
Blocked by: none

## Question

Set the compatibility stance that the rest of the design is written against. The recommendation is **additive**, and the reasons are worth testing:

- **`jjfx pool <N>` keeps working**, sizing a `default` pool. Every existing script and the README quickstart survive, and migration becomes a rename rather than a data migration.
- **`jjfx status --json` keeps its flat `workers[]`** and gains a `pool` field per Worker, rather than nesting under `pools`. That JSON shape changed **today** — bare array to `{"workers": [...], "waiting": [...]}`, shipping in 0.56.0 — and breaking it again within hours would be the worst kind of churn. `jq '.workers[] | select(.pool == "bugs")'` covers the nested query anyway.
- **Worker addressing is unchanged.** Names are random hex and already globally unique, so `send`, `logs`, `mount`, `rebase`, `absorb`, `open-pr`, `pool rm` and `pool reset` need no pool qualification. The blast radius is the pool commands and `dispatch`, not the whole CLI.

Decide whether that stance holds, and whether anything *must* break. If something must, name it and say why, so it lands as one named break rather than two — and check it against the other tickets on this map, since the routing and config decisions can each introduce a break of their own.

## Answer

Resolved 2026-10-16. **The additive stance holds, and nothing must break.**

Every surface checked against the decisions this map has already made:

- **`jjfx pool <N>` keeps working**, sizing `default`. The declaration model (*Where are pools declared?*) makes `default` implicit whenever `[pools]` is absent, so a Repository that never opts in is unchanged — and migration stays a rename rather than a data migration.
- **`status --json` keeps its flat `workers[]`**, gaining a `pool` field; `waiting[]` gains one too. New per-pool information arrives as a *sibling* `pools` key (*How do a pool's spend cap and the global ceiling interact?*), never by nesting. That shape changed the day before this effort began and ships in 0.56.0; breaking it again within days would be the worst kind of churn, and `jq '.workers[] | select(.pool == "bugs")'` already answers the nested query.
- **Worker addressing is unchanged.** Names are random hex and globally unique, so `send`, `logs`, `mount`, `rebase`, `absorb`, `open-pr` and `pool rm <worker>` need no pool qualification. The blast radius stays the pool commands and `dispatch`.
- **Routing is additive** (*How does a Ticket reach a pool?*): one pool implies the target, so no existing invocation changes. `--pool` becomes required only once a second pool exists — a Repository that opted in.
- **The local ticket convention gains a `Labels:` line**, which its parser already ignores.
- **The config gains a `[pools]` section**, and `deny_unknown_fields` still rejects everything else.

**The one thing that is not backward compatible is not a public surface.** The persisted layout moves (`.jj/pool.json` → `.jj/pools/<name>/`), and *Where do pools live on disk, and how does today's pool migrate?* made that an automatic one-time conversion. It is not a break because the `wsg` compatibility contract that once made the layout a contract is dead (*Is the persisted Pool state a `wsg` compatibility contract?*) and jjfx is the only reader.

So: **one stance, zero named breaks.** If a later ticket forces one, it lands here as a named break with its reason rather than as a silent regression.
