# The TUI acts on one named Worker Pool lane

Status: accepted

## Context

ADR 0012 gave a Repository several named Worker Pools and ADR 0013 persisted
each pool's state under its own directory. The CLI followed: `dispatch`, `wait`
and `clean` resolve their lane through one `resolve_pool` seam, and a Repository
holding several refuses an unqualified target rather than silently filling
`default`.

The TUI was left behind. Its dispatch adapter opened `Repository::open`, which
acts on `default`, so every pool-scoped operation - Worker list, resize,
Dispatch - read one lane. Its Ready Ticket discovery took a label from the
caller and read the **global** ready status, so it honoured neither a pool's
routing label nor its per-pool `ready_status`. Nothing in the view named a pool,
so a user driving a multi-lane Repository saw a single-lane view with no sign
that the other lanes existed.

The question is what a multi-lane Repository's TUI does when it needs a lane.

## Decision

**The TUI acts on one named Worker Pool lane at a time, resolved through the
same `resolve_pool` seam the CLI uses.**

- **One lane implies it; several refuse.** At launch the lane is resolved from
  the Repository's declarations. A Repository holding one pool selects it, so a
  single-pool Repository is unchanged. A Repository holding several leaves the
  lane unnamed and the view shows the `resolve_pool` refusal - naming the pools
  - plus a chooser. It never shows a lane's Workers until a lane is named, so the
  silent single-lane view cannot return.
- **`w` chooses the lane.** The chosen lane is shared between the App that names
  it and the dispatch adapter that reads it, so it reaches every pool-scoped
  operation - including the periodic refresh - without rebuilding the
  controller. The view names the lane it acts on in its header.
- **Discovery goes through the CLI's seam.** A lane's routing label and its
  `ready_status` are read through the same `resolve_pool`, `ready_label_for`,
  `pool_ready_status` and `ReadyTicketFilter` code `dispatch` uses, resolved
  once into a `PoolRouting` the adapter holds. The source's fallback label stays
  the global one, so an untagged Ticket cannot match every lane at once.
- **The view is one lane, not all lanes.** A stacked view of every lane's
  Workers would need every action to answer "which lane?", and the CLI's own
  model is one lane at a time. Showing one lane and naming it keeps the TUI and
  the CLI telling the same story.

## Considered options

- **Show every lane at once.** Rejected: actions (resize, destroy, dispatch)
  are lane-scoped, so a combined view would need a lane cursor per action and
  would still have to name a lane. It also invites acting on the wrong lane.
- **Report a multi-lane Repository and stay single-lane.** Rejected by the
  owner: it leaves the TUI unable to drive a Repository the CLI can, and the
  per-pool routing and ready status would stay ignored.
- **Rebuild the controller when the lane changes.** Rejected: the periodic
  refresh poller holds a clone of the controller, so a rebuilt controller would
  leave the poller refreshing the old lane. A shared lane handle keeps one
  controller honest across a change.

## Consequences

The TUI can no longer show a multi-lane Repository as a single lane without
saying so: an unnamed lane is visible, named lanes are named, and a lane's own
routing and ready status decide its work. A single-pool Repository is unchanged,
because one pool still implies itself.

The lane is chosen for the session and changed with `w`; it is not a launch
flag, because the interactive surface is where the choice belongs. A future
change that wants several lanes on screen at once would have to revisit the
one-lane decision here.
