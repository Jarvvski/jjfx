# Is this additive, or a redesign?

Type: grilling
Status: ready-for-human
Blocked by: none

## Question

Set the compatibility stance that the rest of the design is written against. The recommendation is **additive**, and the reasons are worth testing:

- **`jjfx pool <N>` keeps working**, sizing a `default` pool. Every existing script and the README quickstart survive, and migration becomes a rename rather than a data migration.
- **`jjfx status --json` keeps its flat `workers[]`** and gains a `pool` field per Worker, rather than nesting under `pools`. That JSON shape changed **today** — bare array to `{"workers": [...], "waiting": [...]}`, shipping in 0.56.0 — and breaking it again within hours would be the worst kind of churn. `jq '.workers[] | select(.pool == "bugs")'` covers the nested query anyway.
- **Worker addressing is unchanged.** Names are random hex and already globally unique, so `send`, `logs`, `mount`, `rebase`, `absorb`, `open-pr`, `pool rm` and `pool reset` need no pool qualification. The blast radius is the pool commands and `dispatch`, not the whole CLI.

Decide whether that stance holds, and whether anything *must* break. If something must, name it and say why, so it lands as one named break rather than two — and check it against the other tickets on this map, since the routing and config decisions can each introduce a break of their own.

## Answer

<!-- recorded on resolution -->
