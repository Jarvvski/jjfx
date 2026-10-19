# The TUI is still single-pool-aware

Type: task
Status: resolved
Blocked by: none

## Problem

The named Worker Pools effort gave the CLI per-pool routing, per-pool ready status, per-pool spend and a per-pool view - and left the TUI behind. Its discovery reads the **global** ready status (`workspace_dispatch::adapter::discover_ready` takes a label and calls `self.tickets.ready_status()`), so it ignores per-pool routing and the per-pool ready status entirely, and its Worker rows carry no pool dimension.

Two workers flagged this during the effort and both declined to half-wire it, which was the right call: wiring the status alone would have been inconsistent with the label it also ignores. It has never had a ticket, which is how a deferral becomes an oversight.

## Solution

Decide what the TUI should show for a multi-pool Repository and do it as one piece of work rather than field by field: discovery through the same per-pool seam the CLI uses, and a pool dimension on the rows and the pool view. If the answer is that the TUI is intentionally single-pool and should say so, that is also a valid resolution - but it must be recorded, because today it is silent.

## Acceptance Criteria

- [x] The TUI either honours per-pool routing and ready status, or reports a multi-pool Repository explicitly.
- [x] Worker rows or the pool view name the pool a Worker belongs to, or the absence is documented.
- [x] A test covers the chosen behaviour.
- [x] `mise run check` is green.

## References

- `src/workspace_dispatch/adapter.rs` (`discover_ready`), `src/app.rs`, `src/render/pool.rs`
- `.scratch/named-pools/issues/07-route-by-label.md`, `14-per-pool-ready-status-unused.md`, `08-per-pool-spend.md`

## Resolution

The TUI is multi-pool-aware, the larger of the two resolutions. It acts on one
named Worker Pool lane at a time, resolved through the same seam `dispatch` uses,
and it names the lane it acts on.

**One seam, not two.** `resolve_pool`, `global_ready_label` and `ready_label_for`
moved out of `src/cli.rs` into `src/config.rs` as public functions, and
`dispatch`, `wait`, `clean` and the TUI now all call the same code. A new
`PoolRouting` value (in `DispatchSettings`) resolves a Repository's lane set, its
routing table and its per-lane ready status once, and exposes them through
`resolve`, `ready_label` and `ready_status`, which call the moved functions and
`RepoConfig::pool_ready_status`. The adapter's `discover_ready` reads a lane's
label and status from that value, builds the same `ReadyTicketFilter`, and
narrows the `Repository` with `with_pool` - so it no longer reads the global
ready status or a caller-supplied label.

**A multi-lane Repository refuses a lane until one is named.** At launch the lane
is resolved with `resolve_pool`: one pool implies it (every single-pool
Repository is unchanged), several leave it unnamed and the view shows the
`resolve_pool` refusal - naming the pools - plus a chooser. `w` opens the
chooser; `enter` names a lane, which is shared with the adapter through a
`PoolSelection` cell so every operation, including the periodic refresh, reads
the chosen lane. The pool view header names the lane it acts on. See
`docs/adr/0015-tui-acts-on-one-named-lane.md`.

**Verified** by `mise run check` (green) and new tests: `config` covers
`PoolRouting`'s default and its agreement with the `dispatch` seam;
`workspace_dispatch::adapter` scopes two lanes over one local-Ticket tree and
asserts each draws its own work by its own label and ready status; `render::pool`
asserts the view names its lane and that an unnamed lane shows the refusal and
the choices without a lane's Workers; `app` asserts a single-pool Repository
names its lane and a multi-pool one refuses until a lane is chosen, reaching the
adapter's shared handle.

## Notes

The lane is chosen in-session, not by a launch flag, and the view shows one lane
rather than stacking every lane: actions are lane-scoped, so a combined view
would still have to name a lane per action. A future change that wants several
lanes on screen at once has to revisit that decision (recorded in ADR 0015).
