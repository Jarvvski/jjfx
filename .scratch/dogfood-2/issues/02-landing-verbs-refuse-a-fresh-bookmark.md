# Landing verbs refuse a fresh (unpushed) bookmark: the same name counted twice

Status: resolved
Type: bug
Blocked by: none

## Problem

`resolve_ticket_branch` (`src/engine/orchestration.rs:766`) finds a Worker's
bookmark by running

    jj bookmark list -T 'name ++ "\n"'

and treating more than one match as ambiguous. When a remote auto-tracks
bookmarks, a **local bookmark whose remote-tracking counterpart has not been
created yet** is listed **twice** by that template - once with `remote=` and once
with `remote=origin present=false` - so the filter counts the same bookmark as two
and returns `Ambiguous`.

The double-listing is a *configuration*, not a jj default: this drive's user
config set `remotes.origin.auto-track-bookmarks = "glob:*"`, which the fixture's
`origin` picked up. On a clean jj config the same fresh bookmark lists once - which
is why the fix must read the local bookmark (`remote == ""`) rather than assume
either shape.

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

- [x] A single bookmark is never counted as ambiguous because of its
      remote-tracking entry: the read that matches a Ticket's bookmark filters on
      the local bookmark (e.g. `remote == ""`), or otherwise ignores a
      not-yet-created remote ref.
- [x] A freshly dispatched Worker can be landed (`absorb` / `rebase` / `open-pr`)
      without a manual push first.
- [x] A dependent whose Blocker has no *usable* branch is reported as unstacked
      exactly once, and the reason names the bookmark state, not "no branch".

## Resolution

The bookmark read moved behind one local-only helper. `crate::jj::local_bookmark_names`
(`src/jj.rs`) reads `jj bookmark list -T 'if(remote, "", name ++ "\n")'`, so a
bookmark a remote-tracking entry would otherwise list twice is read once.
`resolve_ticket_branch` (`src/engine/orchestration.rs`) now reads
through it, so a freshly dispatched Worker's unpushed bookmark resolves as
`Resolved` rather than reading as `Ambiguous`, and `absorb` / `rebase` /
`open-pr` / `review` land it with no manual `jj git push` first. `bookmark_exists`
reads through the same helper, so a push is classified against local bookmarks
only. A genuine conflict - two *real* bookmarks naming one Ticket - still reads
as `Ambiguous`.

The `--all` stacked base no longer degrades silently to a bare "no branch".
`dependency_basis` now takes the `TicketBranch` outcome rather than an
`Option<String>`, so an unstackable Blocker is reported once, with the reason
naming its bookmark state: `bookmark is missing`, `bookmark is ambiguous (<names>)`,
or `bookmark \`main\` is trunk`. A repeated Blocker declaration is collapsed to a
single report.

Verified with `mise run check` (fmt + clippy + build + test green). New coverage:
`src/jj.rs` asserts the plain `name` template double-lists a fresh bookmark while
`local_bookmark_names` returns one; `src/engine/orchestration.rs` asserts a fresh
bookmark with a configured remote resolves, two real bookmarks stay ambiguous,
and each unstackable-Blocker reason names its bookmark state; `tests/local_tickets.rs`
drives `dispatch --all` and asserts the dependent's unstacked report appears
exactly once, naming the Blocker's missing bookmark. The two double-listing tests
write `remotes.origin.auto-track-bookmarks` into the repository's own config
before creating the bookmark under test, so they construct the tracked-but-absent
state instead of inheriting it from the ambient jj config - the guard then holds
on a clean-config CI runner, not only on a machine that sets the option.

## References

- `src/engine/orchestration.rs` (`resolve_ticket_branch`, the stacked-base
  resolution), `src/jj.rs` (`local_bookmark_names`, `bookmark_exists`)
- `.scratch/dogfood/issues/02-resolve-the-worker-bookmark-before-mutating.md`

## Triage (2026-10-20)

Triaged from `needs-triage` to `ready-for-agent`. The evidence is a **live
reproduction during the second drive**, not a code reading: the drive hit this
while working a real backlog through jjfx, and the report
(`.scratch/dogfood-2/report.md`) records what happened around it.

Independently **confirmed**: in the drive fixture, `jj bookmark list -T name`
printed `adam/sift--03-top-messages` **twice** - with `remotes.origin.auto-track-bookmarks`
in effect, a bookmark with a tracked-but-absent remote entry is listed more than
once, so one bookmark reads as two.

The acceptance criteria are unchanged - they were already specific and
testable, which is why this needed triage rather than a rewrite.
