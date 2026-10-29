# Destroying a pool orphans the Tickets it claimed, with no way back

Status: resolved
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

- [x] Destroying a pool that holds claimed Tickets either releases those claims,
      or refuses while they exist and names them, or records the claims somewhere
      that survives the destroy. **Done in #203**: without `--force` the removal
      is refused and names every claimed Ticket; with it the claims are released.
- [x] If claims do survive a destroy, a verb releases them - `resume`, a
      `ticket` subcommand, or an explicit `--force` path that says what it is
      discarding. **Done in #203**: `--force` releases them, saying which, as part
      of the forced path that already says what it discards.
- [x] A test drives a claimed Ticket through a pool destroy and asserts the
      Ticket can be dispatched again afterwards. **Done in #203**: the
      integration test drives `pool destroy --force` and asserts `ticket list`
      reads the released Ticket as `ready`.
- [x] `resume` distinguishes "nothing was ever queued" from "the queue is gone",
      since those need different recoveries. **Verified in #203**: a destroyed
      Pool's declaration is gone, so `resume --pool <name>` refuses with "no
      Worker Pool named `<name>`" rather than materialising an empty Pool and
      reporting "No unfinished steps recorded" - the two recoveries are distinct.

## Comments

- 2026-10-29: #203 closes this. `--force` is the release path, and it is the same
  flag that already means "I accept what this discards" - a claim is another
  thing a destroy discards, so it is released and said so, not stranded.
