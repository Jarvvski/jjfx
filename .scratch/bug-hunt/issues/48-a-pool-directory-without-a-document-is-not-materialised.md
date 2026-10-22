# A pool state directory without a document is never materialised

Status: needs-triage
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

- [ ] Establish whether a product path can leave `.jj/pools/<name>/` without a
      document - a lock, a Run record, or a partially migrated layout - and
      reproduce it if so.
- [ ] Either materialise on a missing *document* rather than a missing directory,
      or report the state clearly: name the pool, say the document is missing, and
      say that materialisation was skipped.
- [ ] If it is unreachable in the product, record that and close it, as `43` was.
