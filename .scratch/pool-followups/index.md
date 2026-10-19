# Pool follow-ups

The loose ends the named Worker Pools effort left, filed from three sources: the
end-to-end drive of the finished feature, the workers' own reports, and the
reconciliation of tickets that were marked resolved without their criteria
ticked.

None of these block the feature. It works, and the drive proved it; these are the
edges. They are filed rather than left in reports because a report is not a
backlog.

## The tickets

| # | Ticket | Blocked by |
|---|---|---|
| 01 | `jjfx wait` has no pool scope | - |
| 02 | A malformed pool document prints an unnamed diagnostic | - |
| 03 | `pool destroy --force` has no end-to-end test for a live busy Worker | - |
| 04 | The pool-shrink test lost its isolation | - |
| 05 | The Linear branch of the empty-ready-set `root` is untested | - |
| 06 | The TUI is still single-pool-aware | - |
| 07 | Reconcile the remaining batches' unticked acceptance criteria | - |

## The two that matter most

**01 is a correctness gap, not polish.** With several pools `jjfx wait` can act on
only one lane, and nothing refuses or says which - the silent-wrong-pool failure
the implementation deliberately removed from `dispatch`. It is the only ticket
here where a user can get a wrong answer rather than a missing one.

**06 is the largest.** The TUI ignores per-pool routing and the per-pool ready
status entirely, so anyone driving jjfx from it sees a single-pool view of a
multi-pool Repository. It was deferred by design during the effort - twice,
explicitly - and has never had a ticket, which is how a deferral becomes an
oversight.

## 07 is a record problem, not a code problem

Thirteen tickets in the two most recent batches were marked `resolved` with their
acceptance boxes unticked; twelve were confirmed by an independent re-read of the
merged code and one was re-opened. The same drift runs through every older batch,
so `Status: resolved` cannot be trusted as an audit trail until it is reconciled.
The load-sensitive tests are the worked example: the class already has a resolved
ticket, and it flaked again.
