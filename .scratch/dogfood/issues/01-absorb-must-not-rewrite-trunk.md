# `jjfx absorb` must not rewrite trunk

Status: resolved
Type: bug
Blocked by: none

## Problem

`jjfx absorb` runs bare `jj absorb` in the Worker's Workspace
(`src/engine/worker_actions.rs:1149`). `jj absorb` moves the working-copy
commit's hunks into the closest **mutable** ancestor where those lines last
changed, and abandons the source once every hunk has landed.

That is only safe when trunk is immutable, because then absorb stops at trunk.
jjfx never checks, and a Worker's `@` **is** its deliverable commit - so absorb
aims the deliverable at whatever ancestor is mutable.

The precondition is weaker than "a GitHub repository": it needs `trunk()` to
resolve, i.e. a fetched `main@origin`. In the repository this was observed in,
there was an `origin` remote but no remote-tracking `main`, so:

    $ jj log -r 'trunk()'          # 000000000000 - resolves to nothing
    $ jj log -r 'immutable_heads()' # 000000000000 - empty
    $ jj log -r 'mutable() & bookmarks(exact:main)'
    da20d015ead7 main*             # jj itself calls local main mutable

Every commit was mutable, including trunk. What happened next:

    $ jjfx absorb worker-12af32
    Absorbed edits into 1 commit(s) on contact-form--01-form

    $ jj op log --op-diff        # op 11aec05ceb74
    Changed commits:
    + okxoxy b605ec main* | Lantern static site
    - okxoxy/1 d36015 (hidden) Lantern static site
    - tkyqkq/1 c2f121 (hidden) contact-form--01-form: Add a contact form

`main` was rewritten and the Worker's commit abandoned. Repeated over four
Workers it left a partially-applied trunk: the dark-mode button and CSS landed
but not the JS that drives them, `contact-form` landed wholesale, and a new
file (`404.html`, with no ancestor lines to attach to) did not land at all.

A Worker whose change is already committed has nothing in its working copy, so
the expected outcome is a no-op. Today it silently rewrites trunk instead.

## Acceptance Criteria

- [x] `absorb` never moves trunk: after the `jj absorb` step, if trunk moved,
      abort and restore (`jj undo`) rather than reporting success.
- [x] A Worker whose change is already committed is a no-op - nothing lands in
      trunk and the Worker's commit survives.
- [x] The precondition is enforced or checked, not assumed: either the landing
      path establishes an immutable trunk, or it refuses when `trunk()` does
      not resolve.
- [x] A test drives the dangerous case (`trunk()` unresolved, so everything is
      mutable) and asserts trunk is unchanged.

## Out of Scope

- The review-feedback flow that legitimately uses `jj absorb` *inside* a
  workspace (`opinionated-loop/15`); it must keep working.

## References

- `src/engine/worker_actions.rs:1149` (`absorb`), `:520` (`WorkerActions::absorb`)
- `jj help absorb`

## Resolution

Made it impossible rather than merely detected, and kept the detection as a
backstop. Three layers:

1. **The destination is pinned.** `jj absorb` takes `--into <revsets>`
   (default `mutable()`), so `SystemCommands::absorb` now passes
   `crate::jj::absorb_destinations()` -
   `mutable() & ::@ & ~::(<trunk base>)`, the Worker's own mutable commits above
   the mainline. Trunk cannot be a destination whether or not it is immutable,
   so a Worker whose change is already committed has nowhere to fold into and
   the command is the no-op it should be. This also covers the case the
   precondition check alone does not: a local `main` that is *mutable* because
   it is ahead of `main@origin`.
2. **The precondition is checked, not assumed.** `WorkerActions::absorb` reads
   jj's own `trunk() ~ root()` and refuses with
   `WorkerActionError::TrunkUnresolved` when it is empty, naming what is
   missing (no fetched `main@origin`) and the remedy (`jj git fetch`, or push
   `main` to `origin`). Nothing runs, so nothing can be destroyed.
3. **The mainline is read either side of the fold.** If it moved anyway - a
   future jj ignoring `--into`, say - the absorb is rolled back with
   `jj undo --allow-cross-workspace` and reported as a failure; the push is
   skipped and success is never claimed.

Verified: `mise run check` green. Two real-jj unit tests drive
`SystemCommands::absorb` - `absorb_never_folds_a_committed_change_into_a_mutable_main`
(the pin) and `absorb_still_folds_an_edit_into_the_workers_own_base_commit` (the
legitimate fold that must keep working). Two integration tests cover the rest:
`absorb_refuses_when_trunk_does_not_resolve_and_leaves_main_alone` drives the
whole `WorkerActions::absorb` path against a real repository whose `trunk()` does
not resolve, asserting `main`'s commit id is unchanged and the Worker's commit
keeps its change, and
`an_absorb_that_rewrites_the_mainline_is_rolled_back_and_fails` covers the
rollback. Two more pin the jj semantics:
`absorb_destinations_keep_the_worker_stack_and_drop_a_mutable_main` and
`trunk_resolution_is_read_from_jjs_own_revset_not_assumed`. Both dangerous-case
tests were confirmed to fail against the previous behaviour before the fix
landed.

Out of scope, and unchanged: the review prompt still sends a Worker's agent
through bare `jj absorb` *inside* its own Workspace (`opinionated-loop/15`), which
folds an edit commit into a mutable base commit. The
`the_documented_base_commit_flow_lands_feedback_in_the_base_commit` test still
passes.
