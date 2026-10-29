# A Ticket whose status does not match the lane is reported as "blocked"

Status: resolved
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

- [x] A Ticket excluded because its status is not the lane's ready status is
      reported distinctly from one waiting on an unresolved `Blocked by:` - for
      example `not-ready` or `held` versus `blocked`. **Done in #201**: `not-ready`
      versus `blocked`, decided by the diagnostic's reason.
- [x] The `DETAIL` keeps naming the expected status, as it does today. **Held in
      #201** - the reason text is unchanged.
- [x] `--json` carries the two states as distinct values, not one shared string.
      **Done in #201**: `"not-ready"` and `"blocked"` are separate `state` values.
- [x] A test covers a lane with both kinds at once, so the distinction cannot
      regress. **Done in #201** - `tests/ticket_list.rs`'s fixture already had
      both, and the prose test now pins them.

## Comments

- 2026-10-29: #201 closes this. The first attempt made a *resolved* Ticket read
  `not-ready`, because it checked the Status before asking whether the Ready set
  had excluded the Ticket at all; the existing JSON test caught it. The check
  belongs inside the diagnostic branch, where "excluded, and why" is known.
