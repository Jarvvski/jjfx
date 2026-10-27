# `pool reset <worker>` for a Worker in another pool reports "state is missing"

Status: needs-triage
Type: bug
Blocked by: none

## Problem

A Repository with two pools (`default` and `ledger`) has a Worker in `ledger`.
Resetting it without naming the pool looks the Worker up in `default`, does not
find it, and reports a missing *state document* rather than "that Worker is not in
this pool":

```
$ jjfx pool reset worker-bea63d
Error: cannot reset Run for Worker worker-bea63d: Worker worker-bea63d state is missing

Caused by:
    Worker worker-bea63d state is missing
```

The Worker exists, is listed by `jjfx status`, and belongs to another pool. The
message names neither the pool it looked in nor the pool the Worker is actually
in, and "state is missing" reads as corruption rather than "wrong pool".

This is the same unhelpful-error shape as
[`bug-hunt/48`](../../bug-hunt/issues/48-a-pool-directory-without-a-document-is-not-materialised.md),
which was about a pool document rather than a Worker: a lookup that fails says
what it could not find, not which container it searched or what the reader should
do instead.

## Acceptance Criteria

- [ ] The error names the pool that was searched and, when the Worker exists
      elsewhere, the pool it belongs to.
- [ ] A Worker id that exists in no pool is distinguished from one that exists in
      another pool.
- [ ] Whether `pool reset` should accept a `--pool` selector, or infer the pool
      from the Worker id (which is unique across the Repository), is decided and
      documented - today the verb offers neither.
- [ ] A test covers both cases: an unknown Worker, and a Worker in another pool.
