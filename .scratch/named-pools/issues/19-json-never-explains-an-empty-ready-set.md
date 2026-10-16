# `--json` never explains an empty ready set

Type: task
Status: resolved
Blocked by: none

## Problem

Found by driving a throwaway Repository end to end. When nothing is ready, the **prose** form explains why on stderr, but the **`--json`** form prints `[]` and says nothing anywhere:

    $ jjfx dispatch --all
    Fetching tickets with label 'ready-for-agent'...
    No tickets found with label 'ready-for-agent'

    $ jjfx dispatch --all --json
    Fetching tickets with label 'ready-for-agent'...     <- stderr only
    []                                                   <- stdout

This is **pre-existing**, not a pool regression: the single-pool path behaves identically to the pool-scoped and fan-out paths. It matters more now, because the pool work made the machine-readable surface the one people script against. `--json` is how a caller decides what to do next, and `[]` cannot distinguish:

- no Tickets exist,
- Tickets exist but none is ready,
- the label matched nothing,
- the search root is wrong.

The dispatch-ergonomics batch's ready-set work fixed the prose and added `skipped` entries for Tickets that were discovered but routed nowhere. This is the same rule for the case where nothing was discovered at all, and it should reach the JSON as **structure** rather than a stderr aside.

## Solution

Give `--json` the same reason the prose path already has: the label it searched, the root it searched, and the per-Ticket diagnostics when there were Tickets that did not qualify. A sibling key or `skipped` entries are both fine - the point is that a caller can tell why without parsing stderr.

Keep the prose output as it is; this is about the machine-readable half catching up.

## Acceptance Criteria

- [ ] `dispatch --all --json` with nothing ready carries the reason in its JSON, not only on stderr.
- [ ] The pool-scoped and fan-out paths behave the same as the single-pool path.
- [ ] A test covers it.
- [ ] `mise run check` is green.

## References

- `src/cli.rs` (`dispatch_command`, the ready-set diagnostics), `src/engine/local_tickets.rs`
- `.scratch/dispatch-ergonomics/` (the prose half, and the `skipped` entries it added)

## Comments

- 2026-10-16: Resolved. An empty `--all` ready set now carries the search as structure in the `--json` batch: a `skipped` entry with a null `ticket` names the `label` it looked for and the `root` it looked under, ahead of the per-Ticket diagnostics it already reported. The top level stays a bare array, so the change is additive and no consumer has to change; the `root` is the local markdown root for a local source and null for Linear, which has no filesystem root. All three paths carry it - the single-pool path, the pool-scoped `--all --pool <name>` (which names that lane's routing label), and the fan-out (one search entry per routable pool's label) - and the prose output is unchanged. Unit tests cover the search entry rendering alongside a diagnostic and the Linear null root; `tests/dispatch_outcomes.rs` drives the real binary for the genuinely-empty and discovered-but-not-ready cases, and `tests/pool_routing.rs` covers the pool-scoped and fan-out paths. `mise run check` is green and `momo add` records the change.
