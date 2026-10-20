# Landing verbs refuse a fresh (unpushed) bookmark: the same name counted twice

Status: ready-for-agent
Type: bug
Blocked by: none

## Problem

`resolve_ticket_branch` (`src/engine/orchestration.rs:766`) finds a Worker's
bookmark by running

    jj bookmark list -T 'name ++ "\n"'

and treating more than one match as ambiguous. On jj 0.46, a **local bookmark
whose remote-tracking counterpart has not been created yet** is listed **twice**
by that template - once with `remote=` and once with `remote=origin` - so the
filter counts the same bookmark as two and returns `Ambiguous`.

That is exactly the state of a freshly dispatched Worker: the agent created its
bookmark, jjfx has not pushed it, and the remote ref does not exist yet. So
every landing verb refuses, naming the bookmark twice:

    $ jjfx absorb worker-1be12c
    Error: Worker worker-1be12c's Ticket sift--01-bucket-counts is named by more than
    one bookmark (adam/sift--01-bucket-counts, adam/sift--01-bucket-counts): keep one
    and delete the rest

    $ jjfx rebase worker-1be12c      # same error
    $ jjfx open-pr worker-1be12c     # same error
    $ jjfx review worker-1be12c      # same error (it resolves the branch too)

The trigger is visible directly:

    $ jj bookmark list -T 'name ++ " | remote=" ++ remote ++ " | present=" ++ present ++ "\n"'
    adam/sift--01-bucket-counts | remote=       | present=true
    adam/sift--01-bucket-counts | remote=origin | present=false
    adam/sift--03-top-messages  | remote=       | present=true
    adam/sift--03-top-messages  | remote=origin | present=false
    main                        | remote=       | present=true

Pushing the bookmark by hand clears it, and the landing verbs then work:

    $ jj git push -b adam/sift--01-bucket-counts
    $ jjfx rebase worker-1be12c
    Rebased adam/sift--01-bucket-counts

So the verbs fail for precisely the case they exist to serve - landing a branch
the Worker just produced - and the only fix is to push first, which is the step
`rebase` was supposed to do.

**Second effect - dependency stacking silently degrades.** The `--all` path
resolves a dependent's stacked base through the same function, so a Blocker with
an unpushed branch reads as "no branch" and the dependent is based on trunk with
no warning that it is missing the Blocker's code:

    $ jjfx dispatch --all
    ...
    Ticket sift--07-auto-buckets is blocked by sift--01-bucket-counts, which has no
    branch; basing it on trunk

(After the Blocker's branch was pushed, re-dispatching 07 did stack on it - the
stack base recorded `78874ee`, the Blocker's commit.)

## Acceptance Criteria

- [ ] A single bookmark is never counted as ambiguous because of its
      remote-tracking entry: the read that matches a Ticket's bookmark filters on
      the local bookmark (e.g. `remote == ""`), or otherwise ignores a
      not-yet-created remote ref.
- [ ] A freshly dispatched Worker can be landed (`absorb` / `rebase` / `open-pr`)
      without a manual push first.
- [ ] A dependent whose Blocker has no *usable* branch is reported as unstacked
      exactly once, and the reason names the bookmark state, not "no branch".

## References

- `src/engine/orchestration.rs:766` (`resolve_ticket_branch`), `:686`
  (stacked-base resolution)
- `src/jj.rs:660` (the same `bookmark list -T name` read)
- `.scratch/dogfood/issues/02-resolve-the-worker-bookmark-before-mutating.md`

## Triage (2026-10-20)

Triaged from `needs-triage` to `ready-for-agent`. The evidence is a **live
reproduction during the second drive**, not a code reading: the drive hit this
while working a real backlog through jjfx, and the report
(`.scratch/dogfood-2/report.md`) records what happened around it.

Independently **confirmed**: in the drive fixture, `jj bookmark list -T name`
printed `adam/sift--03-top-messages` **twice** - a bookmark with a remote
counterpart is listed more than once, so one bookmark reads as two.

The acceptance criteria are unchanged - they were already specific and
testable, which is why this needed triage rather than a rewrite.
