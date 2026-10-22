# A pool state directory without a document is never materialised

Status: resolved
Type: bug
Blocked by: none

## Problem

`WorkerPool::materialise` decides whether a declared pool still needs
provisioning by asking whether its state **directory** exists:

```rust
if self.repository.state_store().directory_exists() {
    return Ok(Vec::new());
}
```

`directory_exists` is `self.directory().is_dir()`, and `directory()` is
`.jj/pools/<name>/` - a directory that holds the pool document, its Workers, its
Run records, its waiting set and its locks. So *any one* of those is enough to
make jjfx believe the pool is materialised, including a directory created for a
lock or for a Run record that holds no document at all.

Taking a pool lock creates that directory: `atomic::open_lock` calls
`fs::create_dir_all(parent)` before flocking. When the directory exists and the
document does not, the first reservation fails with

```
cannot load Worker Pool state: cannot reserve Worker Pool: state is missing
```

which names neither the pool nor the missing document, and does not say that
materialisation was skipped.

## Evidence, and what is not yet proven

Reproduced only through a *test* fixture so far. Ticket
[47](47-a-cap-fixture-rots-outside-the-rolling-week.md) uncovered it:
`tests/pool_routing.rs::a_capped_lane_is_held_while_other_lanes_dispatch` seeds a
Run record with `seed_spend`, which creates `.jj/pools/bugs/` by hand. Once the
cap stopped refusing, the lane dispatched, materialisation had been skipped, and
the reservation failed with exactly that error.

**Not yet reproduced on a product path.** `materialise_declared_pool` runs after
`spend_gate`, so if any pool-scoped read that runs first takes the pool lock, a
declared lane could be left un-materialised in normal use. Other declared-pool
tests pass, which suggests nothing in the current dispatch path does - so this may
be unreachable today and only a robustness gap. It needs a product-path
reproduction before it is called a bug.

## Acceptance Criteria

- [x] Establish whether a product path can leave `.jj/pools/<name>/` without a
      document - a lock, a Run record, or a partially migrated layout - and
      reproduce it if so.
- [x] Either materialise on a missing *document* rather than a missing directory,
      or report the state clearly: name the pool, say the document is missing, and
      say that materialisation was skipped.
- [x] If it is unreachable in the product, record that and close it, as `43` was.

## Resolution (Thu 22 Oct 2026)

**Reproduced on a product path: the TUI's in-place destroy.** It is reachable
and it is a real defect, so it is fixed at the root rather than reported.

`WorkerPool::destroy` is what the TUI's `D` destroy calls (the CLI's
`pool destroy` calls `remove_pool_with_progress`, which removes the directory
outright). `WorkerPool::destroy` deliberately *leaves* the directory - its
docstring says so: "leaves a Pool that is recreated in place" - and
`finish_pool_destroy` removes every artifact including `pool.json`, so the
survivor is a directory holding only its locks. `materialise` then asked
`directory_exists()`, saw the directory, and skipped provisioning; the first
reservation failed with the bare `cannot reserve Worker Pool: state is missing`.

That the TUI can target a *named declared* lane (the `w lane` chooser) makes this
end to end: declare `[pools.bugs]`, destroy `bugs` in the TUI, then
`jjfx dispatch --pool bugs` fails and the lane stays dead until someone removes
`.jj/pools/bugs/` by hand. The regression test drives exactly that: it declares
the lane, calls `WorkerPool::destroy` (the API the TUI destroy is), asserts the
directory survives with no document, and requires the next Dispatch to
re-materialise and dispatch.

**Ruled out** as the path that leaves the directory behind:

- pool-scoped *reads* - `PoolStateRepository::load`, `WaitingStateRepository::held`,
  `QueueStateRepository::items`, `RunRecordRepository::records` and `pool_cost`
  are all lock-free `fs::read`s, so `status`, `wait`, `ticket reconcile`, the
  `spend_gate` and the `reconcile_runs` before `materialise_declared_pool` in the
  `--all` fan-out create nothing;
- `jjfx pool create` - writes only the `[pools.<name>]` declaration;
- the CLI's `pool destroy` - `remove_pool_with_progress` removes the directory;
- the legacy-layout conversion - it writes only `default`, and only when
  `.jj/pool/` is present; a `.jj/pool/` with no `.jj/pool.json` would create
  `.jj/pools/default/` without a document, but that legacy shape was not
  reproduced and `default` recovers with `jjfx pool <N>`.

**The fix.** `StateStore::pool_document_exists` is the new mark of a materialised
Pool, and both `WorkerPool::materialise` and `cli::materialise_declared_pool` key
off it instead of `directory_exists`. Idempotence is unchanged - a Pool whose
document exists is left untouched, so a second Dispatch never re-provisions it -
and the check is now correct for the other writers into the directory (a lock,
which creates its parents; a Run record; the waiting set). `directory_exists`
remains where the directory, not the document, is the right question:
`resize_declared_pool` and `remove_pool_with_progress`.

The residual error is also made honest, per the ticket's second option: the four
pool-document `state is missing` sites in the reservation paths now use
`StateStore::missing_pool_document`, which names the pool, says the state
document is missing (not materialised), and names the command that creates it -
matching the `set_pool_profile` precedent. A unit test pins the message and the
integration test pins the product path.

`mise run check` green. User-visible, so a `momo fix` entry.

