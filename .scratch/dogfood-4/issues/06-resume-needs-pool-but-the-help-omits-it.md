# `resume` needs `--pool` when several pools exist, but its help does not say so

Status: needs-triage
Type: issue
Blocked by: none

## Problem

`resume` is the only verb that re-runs interrupted work, and its help page
advertises no selector:

```
Dispatch and sessions:
  jjfx resume [<TICKET>...]      Re-run the queue's unfinished steps (interrupted work)
```

Run it in a Repository with more than one pool and it refuses:

```
$ jjfx resume
Error: this Repository holds 2 Worker Pools (default, ledger); name one with --pool <name>
```

The error is good - it names the fix - but it is the only place the requirement
appears. `jjfx help resume` does not mention `--pool`, and neither does the
command list.

That matters more than usual here, because `resume` is the recovery path for a
failed or interrupted Run: an operator reaches for it *because* something has
gone wrong, and a multi-pool Repository is exactly where the drive's fixtures
live. In this session it was reached for after two Runs died on an agent
authentication failure.

## Evidence

```
$ jjfx resume
Error: this Repository holds 2 Worker Pools (default, ledger); name one with --pool <name>

$ jjfx resume --pool ledger
  worker-bea63d (PID 28504) -> ledger--01-date-filtering
  worker-153fcb (PID 28797) -> ledger--02-input-sources
  ...
Resumed 5 step(s) in pool 'ledger': ...
```

## Acceptance Criteria

- [ ] `jjfx help resume` documents `--pool`, including that it is required when
      the Repository holds more than one pool.
- [ ] The command list shows the selector, as it does for `ticket list [--pool]`.
- [ ] Either every pool-scoped verb takes `--pool` consistently, or the set that
      does not is deliberate and stated - `status` has no such flag while
      `ticket list` does, and `pool profile` addresses only `default`.
