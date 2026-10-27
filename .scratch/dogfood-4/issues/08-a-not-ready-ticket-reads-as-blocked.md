# A Ticket whose status does not match the lane is reported as "blocked"

Status: needs-triage
Type: issue
Blocked by: none

## Problem

`jjfx ticket list` reports a Ticket as `blocked` when its `Status:` simply is not
the lane's ready status. Ticket 05 in the drive-4 fixture is deliberately
`ready-for-human` - a design decision an agent must not implement - and it is
reported as blocked:

```
TICKET                           STATUS           STATE    DETAIL
ledger--05-multi-currency-policy ready-for-human  blocked  expected status "ready-for-agent", found "ready-for-human"
```

The `DETAIL` is accurate and the behaviour is right: the Ticket must not be
dispatched. The **word** is wrong. "Blocked" already means something specific in
this vocabulary - a `Blocked by:` dependency that is not resolved - and it is
reported that way for tickets 07 and 08 in the same table:

```
ledger--07-monthly-summary  ready-for-agent  blocked  blocked by ledger--01-date-filtering (ready-for-agent)
```

So one column carries two different meanings, and an operator scanning for
"what is blocked" cannot tell "waiting on another Ticket" from "not this lane's
work". A human-owned Ticket is exactly the kind of thing a dispatcher should be
able to pick out at a glance.

## Acceptance Criteria

- [ ] A Ticket excluded because its status is not the lane's ready status is
      reported distinctly from one waiting on an unresolved `Blocked by:` - for
      example `not-ready` or `held` versus `blocked`.
- [ ] The `DETAIL` keeps naming the expected status, as it does today.
- [ ] `--json` carries the two states as distinct values, not one shared string.
- [ ] A test covers a lane with both kinds at once, so the distinction cannot
      regress.
