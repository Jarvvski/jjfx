# `pool reset <worker>` for a Worker in another pool reports "state is missing"

Status: resolved
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

- [x] The error names the pool that was searched and, when the Worker exists
      elsewhere, the pool it belongs to. **Done in #205**: the Worker is now
      *found* in whichever Pool holds it, so there is no error to mislead; an id
      no Pool holds names every Pool that was searched.
- [x] A Worker id that exists in no pool is distinguished from one that exists in
      another pool. **Done in #205**: the first is refused by name with the list
      of Pools searched; the second simply works, and says which Worker it reset.
- [x] Whether `pool reset` should accept a `--pool` selector, or infer the pool
      from the Worker id (which is unique across the Repository), is decided and
      documented - today the verb offers neither. **Decided in #205**: infer. A
      Worker id is unique across the Repository, so the id *is* the selector;
      `--pool` would be a second way to name the same thing and a way to name it
      wrongly. Documented on `jjfx help pool reset` and `pool rm`.
- [x] A test covers both cases: an unknown Worker, and a Worker in another pool.
      **Done in #205** - `tests/pool_removal.rs` drives both, for `reset` and for
      `rm`.

## Comments

- 2026-10-29: #205 fixes `pool reset` and its sibling `pool rm`. Both looked the
  Worker up only in the selected Pool, so a Worker one lane over read as a
  missing *state document* - corruption, rather than the wrong Pool. The wider
  pattern is noted but not fixed here: `send`, `logs`, `mount` and `absorb` take
  a Worker id and are pool-scoped too, and would resolve the same way.
