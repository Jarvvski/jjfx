# The cache-provisioning concurrency test blocks past its budget under load

Status: ready-for-agent
Type: bug
Blocked by: none

## Problem

`tests/repository.rs::concurrent_provisioning_neither_deadlocks_nor_corrupts_the_cache`
exceeded its 120s liveness budget and failed, on **two independent occasions
under heavy artificial load** - the harness used to reproduce `35` and `42`
(many concurrent test binaries plus CPU burners). It passed in isolation (0.57s)
and on the settled re-run, both times.

This is **not** a missed budget. `tests/repository.rs` already uses the shared
`jjfx::test_support::LIVENESS_TIMEOUT` that `35` introduced, so the 120s applied.
The test *blocked* for far longer than a slow operation, which is a different
shape from the two flakes already fixed:

- `35` - a private liveness budget the shared one did not reach.
- `42` - the child refusing to run at all (the terminal-eligibility check
  starving past `LS_QUERY_TIMEOUT`), so empty stdout was by design.
- This one - the work itself not completing.

## Leading hypothesis (unconfirmed)

Many concurrent test binaries provisioning **the same on-disk cache** contend on
it, so one binary's `jj workspace add` queues behind the others. Test it first:
run this binary N-way concurrently (as `42` did) and see whether the wall time
scales with N, or run it while another process holds the cache.

If that is the mechanism it is a test-isolation problem - the tests share a
cache root - not a product bug, and the fix is a cache root per test binary.

## Acceptance Criteria

- [ ] The blocking is reproduced, or the hypothesis is ruled out, with evidence.
- [ ] If it is contention on a shared cache: the tests are isolated so they no
      longer contend, and the `42` load harness passes.
- [ ] If it is a genuine deadlock or lock-ordering bug in the product: the test
      is a red case and the product is fixed.
- [ ] If it cannot be reproduced: recorded and closed, naming what was ruled out.

## References

- `tests/repository.rs` (`concurrent_provisioning_neither_deadlocks_nor_corrupts_the_cache`)
- `.scratch/bug-hunt/issues/35-load-sensitive-helper-deadlines.md`
- `.scratch/bug-hunt/issues/42-flaky-process-and-exec-tests.md`
