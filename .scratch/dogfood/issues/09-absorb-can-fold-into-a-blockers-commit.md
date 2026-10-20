# `absorb` can still fold a stacked Worker's change into its blocker's commit

Status: resolved
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

- [x] Absorbing a stacked Worker never folds its change into a commit belonging
      to another Ticket.
- [x] The legitimate fold still works: an edit commit on top of the Worker's own
      base commit still lands in that base commit
      (`absorb_still_folds_an_edit_into_the_workers_own_base_commit`).
- [x] A test drives a stacked Worker (`02` on `01`) and asserts `01`'s commit is
      unchanged.

## References

- `src/jj.rs::absorb_destinations`, `src/engine/worker_actions.rs` (`absorb`)
- `.scratch/dogfood/issues/01-absorb-must-not-rewrite-trunk.md`
- `.scratch/dispatch-ergonomics/issues/06-stack-dependents-on-blockers.md`

## Resolution

The stacked base is **recorded**, not re-derived. Re-deriving it from the
Ticket's blocker would need a Ticket source (a Linear network call on a landing
verb) and a Blocker branch that still resolves - and it would fail *open*
exactly when the exclusion matters most, once the Blocker has merged and its
branch is gone. Recording it where it is known costs one read and cannot fail
open.

- **Recorded at materialisation.** `prepare_for_dispatch` reads `parents(@)`
  straight after the `jj new <base>` it already runs and stores the commit ids
  in a new jjfx-owned sidecar, `.jj/pools/<pool>/stack-bases.json` (the
  `SessionState` pattern, kept out of the pool and Worker documents). The
  parents are exactly the revisions the Worker was based on: the mainline when
  nothing is stacked, or one commit per Blocker when it is, so a multi-Blocker
  merge records every parent. Recording is uniform and replacing, so a
  re-dispatched Worker never keeps a stale base.
- **Excluded from the destination.** `absorb_destinations(stack_base)` appends
  `& ~::(<base>)` (the union for several bases) to the existing trunk exclusion,
  so the destination is the Worker's own mutable commits above both the mainline
  and its stack base. A Blocker's commit can no longer take the fold, whether or
  not it is mutable.
- **Read on the absorb path.** `WorkerActions::absorb` reads the recorded base
  and passes it to `SystemCommands::absorb`, which builds the revset. An
  unreadable sidecar is an error rather than a silent absence, so the safety
  record fails closed.

Verified: `mise run check` green. The test that proves the fix is
`absorb_never_folds_a_stacked_workers_change_into_its_blockers_commit` (real jj,
`02` stacked on `01`, each on its own bookmark): the feedback edit touches a
line `01` introduced, which is the hunk the old destination folded into `01` -
the test was confirmed to fail against the previous behaviour, with `01`'s
bookmark moving. The legitimate fold is unchanged and still green
(`absorb_still_folds_an_edit_into_the_workers_own_base_commit`, called with no
base). Four more pin the rest:

- `preparing_a_dependent_records_its_blockers_commit_as_the_stack_base` - the
  recording end to end.
- `absorb_excludes_the_recorded_stack_base_from_the_destination` - the wiring
  from state to revset.
- `stack_base_sidecar_records_reads_and_replaces_worker_bases` - the sidecar.
- `absorb_destinations_keep_the_worker_stack_and_drop_a_mutable_main` - an
  unstacked destination still reaches a Blocker's commit (the bug); a stacked
  one does not.

Out of scope, and unchanged: the mainline-moved backstop from `01` still only
checks the mainline. A Blocker's commit being rewritten by a future jj that
ignores `--into` is not separately detected; the pin is the mechanism.

