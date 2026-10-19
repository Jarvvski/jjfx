# `absorb` can still fold a stacked Worker's change into its blocker's commit

Status: ready-for-agent
Type: bug
Blocked by: none

## Problem

`dogfood/01` pinned `jj absorb`'s destination so it can never be trunk
(`src/jj.rs::absorb_destinations`):

    mutable() & ::@ & ~::(<trunk base>)

That closes the trunk case. It leaves a narrower one open: for a Worker stacked
on another Ticket's commit, the **blocker's** commit is itself above trunk and
mutable, so it is a legal destination. Absorbing then folds the dependent
Ticket's change into the blocker's deliverable - the same class of corruption
one level down, rewriting a change that belongs to another Ticket's pull
request.

This is reachable in the normal flow: dispatch materialises a dependent Ticket
as a real stacked workspace (`dispatch-ergonomics/06`), and the landing verbs
are documented as routine. The destination is computed from "the Worker's own
mutable commits above the mainline", which cannot by itself distinguish the
Worker's commits from the ones it was stacked on.

A possible direction: the stacked base is known when the dependent is
materialised, so either record it or re-derive it from the Ticket's blocker,
and exclude it - the same way trunk is excluded now.

## Acceptance Criteria

- [ ] Absorbing a stacked Worker never folds its change into a commit belonging
      to another Ticket.
- [ ] The legitimate fold still works: an edit commit on top of the Worker's own
      base commit still lands in that base commit
      (`absorb_still_folds_an_edit_into_the_workers_own_base_commit`).
- [ ] A test drives a stacked Worker (`02` on `01`) and asserts `01`'s commit is
      unchanged.

## References

- `src/jj.rs::absorb_destinations`, `src/engine/worker_actions.rs` (`absorb`)
- `.scratch/dogfood/issues/01-absorb-must-not-rewrite-trunk.md`
- `.scratch/dispatch-ergonomics/issues/06-stack-dependents-on-blockers.md`
