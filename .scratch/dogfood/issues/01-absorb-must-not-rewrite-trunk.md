# `jjfx absorb` must not rewrite trunk

Status: ready-for-agent
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

- [ ] `absorb` never moves trunk: after the `jj absorb` step, if trunk moved,
      abort and restore (`jj undo`) rather than reporting success.
- [ ] A Worker whose change is already committed is a no-op - nothing lands in
      trunk and the Worker's commit survives.
- [ ] The precondition is enforced or checked, not assumed: either the landing
      path establishes an immutable trunk, or it refuses when `trunk()` does
      not resolve.
- [ ] A test drives the dangerous case (`trunk()` unresolved, so everything is
      mutable) and asserts trunk is unchanged.

## Out of Scope

- The review-feedback flow that legitimately uses `jj absorb` *inside* a
  workspace (`opinionated-loop/15`); it must keep working.

## References

- `src/engine/worker_actions.rs:1149` (`absorb`), `:520` (`WorkerActions::absorb`)
- `jj help absorb`
