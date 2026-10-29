# `resume` needs `--pool` when several pools exist, but its help does not say so

Status: needs-triage
Type: issue
Blocked by: none

## Problem

**Corrected 2026-10-29.** This finding's original claim - that `jjfx help resume`
does not mention `--pool` - is **wrong**, and checking it against the released
binary is what caught it. `jjfx help resume` lists the flag and explains it:

```
Flags:
  --pool <NAME>            Worker Pool whose queue to resume; an explicit one means only this lane
...
A Repository with one Worker Pool implies it, so `--pool` is optional, and one
with several requires it, exactly as `dispatch` does.
```

What *is* true is narrower, and it is the command list only. `resume`'s entry
showed no selector while the entries beside it showed theirs (`wait ... [--until C]`):

```
Dispatch and sessions:
  jjfx resume [<TICKET>...]      Re-run the queue's unfinished steps (interrupted work)
```

Run it in a Repository with more than one pool and it refuses:

```
$ jjfx resume
Error: this Repository holds 2 Worker Pools (default, ledger); name one with --pool <name>
```

So the requirement was documented where a reader of the help page finds it, and
absent from the one-line list an operator skims to find the verb at all.

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

- [x] `jjfx help resume` documents `--pool`, including that it is required when
      the Repository holds more than one pool. **Already true on `0.56.0`** - this
      criterion was met before the finding was filed; see the correction above.
- [x] The command list shows the selector, as it does for `ticket list [--pool]`.
      **Done**: `resume [<TICKET>...] [--pool NAME]`.
- [ ] Either every pool-scoped verb takes `--pool` consistently, or the set that
      does not is deliberate and stated - `status` has no such flag while
      `ticket list` does, and `pool profile` addresses only `default`.

## Comments

- 2026-10-29: The finding was half wrong, and the drive's own discipline is what
  caught it - the claim was never checked against the binary before it was filed.
  `jjfx help resume` has listed `--pool`, in both the flags and the prose, for as
  long as the flag has existed. What remained was the command list, now fixed, and
  the wider question of which verbs take the selector, which is still open.
