# Destroying a pool orphans the Tickets it claimed, with no way back

Status: needs-triage
Type: bug
Blocked by: none

## Problem

A Run claims its Ticket by writing `Status: claimed` into the Ticket file, and the
queue that would release it lives in the pool's state. Destroying the pool removes
the state and leaves the Ticket files claimed, so the Tickets are stranded: they
are no longer `ready-for-agent`, no queue entry knows about them, and no verb
releases them.

The only documented recovery for interrupted work is `resume`, and it has nothing
to resume:

```
$ jjfx pool destroy --force ledger
Removed pool `ledger` from .../.jjfx/config.toml

$ jjfx resume --pool ledger
  Created worker-7b7847
  Created worker-228dcc
  Created worker-922174
  Created worker-72a084
No unfinished steps recorded in pool 'ledger'
```

Meanwhile every Ticket still reads:

```
$ grep -h -m1 '^Status:' .scratch/ledger/issues/*.md | sort | uniq -c
   5 Status: claimed
   1 Status: ready-for-human
   2 Status: ready-for-agent
```

and `dispatch --all` will not touch them, because they are not ready. Recovering
meant editing five Ticket files by hand.

The failure mode is a **destroy** - an operation an operator reaches for
precisely when something is wrong - so it is the worst time to leave the
repository in a state no command can repair.

## Acceptance Criteria

- [ ] Destroying a pool that holds claimed Tickets either releases those claims,
      or refuses while they exist and names them, or records the claims somewhere
      that survives the destroy.
- [ ] If claims do survive a destroy, a verb releases them - `resume`, a
      `ticket` subcommand, or an explicit `--force` path that says what it is
      discarding.
- [ ] A test drives a claimed Ticket through a pool destroy and asserts the
      Ticket can be dispatched again afterwards.
- [ ] `resume` distinguishes "nothing was ever queued" from "the queue is gone",
      since those need different recoveries.
