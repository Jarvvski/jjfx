# Run records

Every Run jjfx finalizes appends one durable record of what it cost and used. The
record exists because jjfx parses a Run's cost, usage, duration and turns and
would otherwise keep them nowhere: the Worker document holds only
status, exit code and error, and the Run log is truncated by the next Run, so
only a Worker's most recent Run would survive.

## Where it lives

The store is `.jj/pools/<name>/runs.jsonl`, inside the pool's own state
directory. It is jjfx-owned and deliberately kept out of the Pool and Worker
documents.

It is one JSON object per line, in append order. The store is **append-only and
never truncated or rewritten**: each line is written under the store's own
sidecar lock (`.jj/pools/<name>/.runs.lock`) and flushed before the lock is
released, so concurrent Workers append whole records rather than interleaving a
partial line.

A line that cannot be decoded - a torn tail from a process that died mid-append,
or a foreign writer - is skipped when reading, so one bad line never hides the
rest.

## The record

Each line carries:

- `run_id` - the Run's identity: its Worker and the timestamp its Reservation
  recorded, the same Run-owned values a Reset compares to tell one Run from the
  next.
- `worker`, `ticket`, `workspace`, `branch` - where the Run ran and for what.
- `runtime`, `provider`, `model` - what ran it.
- `cost_micro_usd` - the provider-reported cost, in millionths of one US dollar.
- `usage` - input, cached-input, cache-write-input, output and reasoning-output
  tokens.
- `duration_ms`, `turns` - the provider-reported duration and turn count.
- `conclusion`, `error` - whether the Run succeeded or failed, and why.
- `started_at`, `completed_at` - when the Run began and ended.

Every provider-reported value is optional. An unrepresentable or missing cost
means "no cost reported", the way bug-hunt ticket 28 handles a bad provider cost
at parse time: the record keeps the Run's outcome and leaves the cost absent,
never treating it as a reason for the Run to fail or the record to be dropped. A
Run with no usable cost still appends a record, and a reader aggregates the costs
that are present.

## Reading it

The store is readable without the TUI. It is the input to the pool cost/usage
view, and to anything else that needs Run history:

```bash
jq -s 'map(.cost_micro_usd // 0) | add / 1000000' .jj/pools/<name>/runs.jsonl
```

Because the store is append-only and carries `completed_at`, a reader can total
spend for a day, a week or a batch without consulting each Worker or the agent
runtime's own session database.
