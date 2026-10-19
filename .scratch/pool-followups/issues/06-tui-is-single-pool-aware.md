# The TUI is still single-pool-aware

Type: task
Status: ready-for-agent
Blocked by: none

## Problem

The named Worker Pools effort gave the CLI per-pool routing, per-pool ready status, per-pool spend and a per-pool view - and left the TUI behind. Its discovery reads the **global** ready status (`workspace_dispatch::adapter::discover_ready` takes a label and calls `self.tickets.ready_status()`), so it ignores per-pool routing and the per-pool ready status entirely, and its Worker rows carry no pool dimension.

Two workers flagged this during the effort and both declined to half-wire it, which was the right call: wiring the status alone would have been inconsistent with the label it also ignores. It has never had a ticket, which is how a deferral becomes an oversight.

## Solution

Decide what the TUI should show for a multi-pool Repository and do it as one piece of work rather than field by field: discovery through the same per-pool seam the CLI uses, and a pool dimension on the rows and the pool view. If the answer is that the TUI is intentionally single-pool and should say so, that is also a valid resolution - but it must be recorded, because today it is silent.

## Acceptance Criteria

- [ ] The TUI either honours per-pool routing and ready status, or reports a multi-pool Repository explicitly.
- [ ] Worker rows or the pool view name the pool a Worker belongs to, or the absence is documented.
- [ ] A test covers the chosen behaviour.
- [ ] `mise run check` is green.

## References

- `src/workspace_dispatch/adapter.rs` (`discover_ready`), `src/app.rs`, `src/render/pool.rs`
- `.scratch/named-pools/issues/07-route-by-label.md`, `14-per-pool-ready-status-unused.md`, `08-per-pool-spend.md`
