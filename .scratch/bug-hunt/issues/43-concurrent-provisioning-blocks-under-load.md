# The cache-provisioning concurrency test blocks past its budget under load

Status: resolved
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

- [x] The blocking is reproduced, or the hypothesis is ruled out, with evidence.
- [ ] If it is contention on a shared cache: the tests are isolated so they no
      longer contend, and the `42` load harness passes.
- [ ] If it is a genuine deadlock or lock-ordering bug in the product: the test
      is a red case and the product is fixed.
- [x] If it cannot be reproduced: recorded and closed, naming what was ruled out.

## References

- `tests/repository.rs` (`concurrent_provisioning_neither_deadlocks_nor_corrupts_the_cache`)
- `.scratch/bug-hunt/issues/35-load-sensitive-helper-deadlines.md`
- `.scratch/bug-hunt/issues/42-flaky-process-and-exec-tests.md`
- `.scratch/bug-hunt/load-harness.sh` (reconstructed)

## Resolution (Tue 20 Oct 2026)

Landed on two of the tree's four boxes, because two of them describe this
outcome: the first is "reproduced **or** the hypothesis is ruled out" and the
last is the terminal action when neither a reproduction nor a defect is found.
The hypothesis is ruled out (box 1, with the evidence below) and the blocking
could not be reproduced (box 4). Boxes 2 and 3 are false, so they stay
unticked: this is not contention on a shared *cache*, and there is no deadlock
or lock-ordering bug. Nothing was changed in `src/`; no changelog entry.

### The hypothesis is ruled out

Each test builds its own repository under a fresh `tempfile::tempdir()`, and
every lock and cache the provisioning path touches is rooted inside it -
`.jj/.repository.lock`, `.jj/ws-cache.lock`, `.jj/ws-cache` and the Worker
state under `.jj/pools/`. Two concurrent test binaries therefore share no lock
or cache path, so there is nothing for them to contend on.

Two experiments agree:

- **Isolating `TMPDIR` per copy changes nothing.** 16 concurrent copies, shared
  `TMPDIR`: min 12.0s / max 14.1s. Each copy given its own `TMPDIR`: min 7.9s /
  max 12.9s. If a shared on-disk cache were the coupling, removing it would
  have collapsed the scaling; it did not.
- **The `mise` shim's shared state is not it either.** Putting the real `jj`
  binary ahead of the shim on `PATH` left 16 copies at 12.2s against 13.0s
  through the shim.

The scaling is also **sub-linear** for the test at moderate N (1 copy 1.5s, 4
copies 3.2s, 8 copies 9.8s, 16 copies 14.1s, 32 copies 21.9s) - the shape of
resource contention, not of a lock queue.

### What is actually shared, and serialised

`jj workspace add` signs the new workspace's working-copy commit. The engine
drops signing when `cfg!(test)` or `JJFX_TEST_SIGNING_OFF` is set
(`src/jj.rs:814`), and `mise run test` sets the variable - but a test binary run
directly does not, so it signs. On this machine the key is on a **smartcard**
(`gpg --list-secret-keys` reports `Card serial no. = 0006 18610587`), and a
fresh repo plus a bare `jj workspace add` does produce a `SIGNED`
working-copy commit. That one device serialises every concurrent sign.

Measured, N concurrent `jj workspace add` in N separate repos:

| N | signing on | signing dropped |
| --- | --- | --- |
| 1 | 2.19s | 0.53s |
| 8 | 3.14s | 1.64s |
| 16 | 5.54s | 1.36s |
| 32 | 10.81s | 2.74s |
| 64 | 21.66s | 4.88s |

Linear when signing (`~0.33s x N`, the signature of one serialised resource),
flat-ish when not. The named test follows it: 16 concurrent copies take 12.0s
with signing against 3.5s without, and 128 copies 82.9s against 26.2s.

This is the shape the ticket describes - "one binary's `jj workspace add`
queues behind the others" - with a different shared resource than the guess.
The gate is unaffected because it sets `JJFX_TEST_SIGNING_OFF=1`.

### The blocking is not reproducible

~350 runs of the named test under load, **0 failures**:

- the ticket-42 harness (16 `adhoc_ticket` binaries + 3 continuous unit-test
  binaries + 48 CPU burners on this 12-core machine, load average peaking at
  186): 6 rounds x 16 copies, plus 3 rounds x 6 whole-test-file copies, max
  named-test wall 36s;
- the named test N-way concurrently with signing on: N = 96 (max 61.7s),
  128 (82.9s), 192 (123.5s), 256 (164.2s).

The reason the budget survives is in the test itself: it is two sequential
`recv_timeout(120s)` calls, so the failure needs the gap between the two sends
- one serialised `jj workspace add` - to exceed 120s, not the whole test. That
gap was 23s (N=96), 31s (N=128), 64s (N=192) and 64s (N=256): it saturates at
about half the budget, so sustained contention cannot red this test at any
concurrency this machine can run.

### Ruled out, and the residual

- No shared on-disk cache, and no `mise`-shim shared state (above).
- No product deadlock or lock-ordering bug. Provisioning takes repository, then
  pool, then cache locks **sequentially and never nested**
  (`workspace.rs::add_workspace_with_revision` drops the repository lock before
  `state.commit` and `project_cache`), each is a per-repository sidecar, and
  `engine::locks` pins the ordering.

What is left is a *transient* stall in a single `jj workspace add`, which is
consistent with both sightings and with the plateau above. The only
load-coupled shared resource in the path is the smartcard, so that is the most
likely transient - but it was not observed, so it is recorded rather than
fixed.

### Harness

`.scratch/bug-hunt/load-harness.sh` reconstructs the "42 load harness", which
only ever existed as ad-hoc shell (ticket 42's resolution describes it, no
script was ever committed). It takes a `signing` knob so both the gate's
configuration (`off`) and the ad-hoc harness's (`on`) are reproducible, and it
passes: 4 rounds x 16 copies x (3 unit binaries + 48 burners), 64 runs, 0
failures, max copy 13s with `signing=off`.
