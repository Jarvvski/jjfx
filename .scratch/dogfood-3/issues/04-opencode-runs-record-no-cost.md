# An opencode Run records no cost, so the `[spend]` cap cannot bind

Status: resolved
Type: bug
Blocked by: none

## Problem

`[spend] cap_usd` is enforced from "the durable per-Run cost records jjfx writes
at finalization" (`docs/configuration.md`, `[spend]`). For an opencode pool those
records carry no cost at all, so the cap is inert: it can never be reached, and
`on_exceed` can never fire.

```
$ jjfx status
Pool: 3 idle, 0 busy, 1 done, 0 failed (4 total)
Spend: $0.0000 today, $0.0000 last 7 days, $0.0000 total across 8 Run(s)
       (8 without a reported cost); tokens in 0 out 0
```

Eight Runs - real opencode sessions that each implemented, tested and described a
change - are reported as having cost nothing. The durable record confirms it is
not a display problem:

```
$ tail -1 .jj/pools/default/runs.jsonl
{
    "run_id": "worker-348336@2026-10-21T09:52:52Z",
    "worker": "worker-348336",
    "ticket": "tally--08-top-n-groups",
    "runtime": "opencode",
    "conclusion": "succeeded",
    "check": "passed",
    "started_at": "2026-10-21T09:52:52Z",
    "completed_at": "2026-10-21T09:58:12Z"
}
```

There is no cost field and no token count. Every key the store holds is listed
above; `cost_usd` and the token fields simply are not written for this runtime.

The honest half is already there - `(8 without a reported cost)` says exactly
what happened, and `jjfx status --json` reports no spend. The dishonest half is
the arithmetic around it: a total of `$0.0000` reads as "these Runs were free",
and a `cap_usd` set on such a pool silently never binds. An operator who sets a
cap to bound agent spend gets no signal that it is not being enforced.

## Acceptance Criteria

- [x] Either an opencode Run's cost is recorded, or jjfx says plainly that a spend
  cap cannot be enforced for that runtime - naming it at the point the cap is
  configured or reported, not only as a footnote on a total.
- [x] A pool whose Runs report no cost does not present `$0.0000` as its spend
  without saying that the figure is not a measurement.
- [x] If the cap cannot bind, a configured `cap_usd` says so rather than appearing
  to be in force.
- [x] `docs/configuration.md`'s `[spend]` section agrees with whichever behaviour is
  chosen.

## Triage (2026-10-21)

Triaged from `needs-triage` to `ready-for-agent`. The evidence is a **live
observation during the third drive**: `jjfx status` after eight opencode Runs
read `8 Run(s) (8 without a reported cost)`.

Independently **confirmed**: the per-Run cost store appends a record for every
Run, including one that reported no cost (`src/engine/state.rs:482`), and
`[spend] cap_usd` is enforced from those records (`docs/configuration.md`), so a
runtime that reports no cost can never reach the cap.

The acceptance criteria are unchanged, and they deliberately allow either answer:
record the cost, or say plainly that the cap cannot bind for that runtime. What
they forbid is `$0.0000` presented as a measurement.

## Resolution

**Chosen: record the cost.** The data is reachable, so the honest answer is to
measure it rather than to declare the cap unenforceable.

### What was found

The lead was chased against a live `opencode` (v2.1.1) daemon, not assumed:

- `session.get` / `session.list` return a session-level `cost` and `tokens`
  (`TokenUsage.Info`), the provider's own **cumulative** total for the session.
  jjfx already called `session.get` at reconciliation but threw both fields away
  (`SessionInfo::into_session`).
- `session.message.list` also carries `cost`/`tokens` on each completed
  assistant message, but summing them does **not** reproduce the session's own
  total (189 assistant messages summed to `$0.7488` against a session `cost` of
  `$0.7966`), and the page size caps at 200. The session total is the faithful
  number, so that is what jjfx records.

### Why the Run's cost was absent

A background opencode Run whose prompt the daemon accepts is **session-delivered
and writes no process log** (`deliver_session_prompt` truncates the log). The
existing `with_opencode_totals` sums `step_finish` events from the Run *log*, so
a session-delivered Run summed an empty log and recorded nothing. Reconciliation
(`opencode_session_state`) had the session in hand, with its cost, and dropped it.

### The change

- `Session` now carries `cost_micro_usd` and `usage`, read from the API.
- Because a session total is cumulative, the session sidecar records the spend
  the session already carried when the Run started (a **baseline**), so a Run
  that resumed a session records only its own spend rather than re-counting its
  earlier Runs. A fresh session's baseline is zero.
- At finalization the Run's own session spend (total − baseline) fills in cost
  and tokens **only where the Run log reported none**, so a process-backed Run
  keeps its per-step log totals and nothing is double-counted.
- A Run whose runtime reports no usable cost is still recorded unpriced.

### Honest reporting (criteria 2 and 3)

Recording the cost makes the cap bind for opencode, but a window can still
measure nothing (a runtime that reports no cost, or an unreadable session). So:

- `PoolCost::is_unmeasured` names that case, and `jjfx status` / the pool view
  say `the total is not a measurement` instead of presenting `$0.0000` as spend.
- A configured cap over an all-unpriced window reports that it **cannot be
  enforced** rather than appearing to be in force. It proceeds rather than
  refusing: there is no evidence of spend to refuse on, and blocking forever on
  an unknown would halt all work.

`docs/configuration.md`'s `[spend]` section was updated to match.
