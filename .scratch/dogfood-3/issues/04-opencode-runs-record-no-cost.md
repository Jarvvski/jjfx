# An opencode Run records no cost, so the `[spend]` cap cannot bind

Status: needs-triage
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

- Either an opencode Run's cost is recorded, or jjfx says plainly that a spend
  cap cannot be enforced for that runtime - naming it at the point the cap is
  configured or reported, not only as a footnote on a total.
- A pool whose Runs report no cost does not present `$0.0000` as its spend
  without saying that the figure is not a measurement.
- If the cap cannot bind, a configured `cap_usd` says so rather than appearing
  to be in force.
- `docs/configuration.md`'s `[spend]` section agrees with whichever behaviour is
  chosen.
