# `jjfx pool profile` should work before the pool exists

Status: resolved
Type: bug
Blocked by: none

## Problem

Setting the runtime profile on a repository with no pool state yet fails with
an opaque error:

    $ jjfx pool profile opencode
    Error: cannot load Worker Pool state: cannot set profile Worker Pool: state is missing

    Caused by:
        cannot set profile Worker Pool: state is missing

`jjfx pool 4` first, then the profile, works. "state is missing" names neither
the state nor why it is needed, and the skill's "Before dispatching" checklist
walks you straight into it by listing the runtime before pool capacity.

## Acceptance Criteria

- [x] `jjfx pool profile <name>` works on a repository with no pool state yet
      (provisioning what it needs), or
- [x] it refuses with a message naming the missing state and the command that
      creates it.
- [x] The skill's ordering matches whichever behaviour is chosen - see
      `.scratch/dogfood/issues/07-skill-preconditions-and-ordering.md`.

## References

- `skills/jjfx-dispatch/SKILL.md` ("Before dispatching")

## Resolution

**Chose the first branch: provisioning.** `jjfx pool profile <runtime>` on a
Repository with no Pool state now creates the empty Pool document it needs and
records the profile, so the drive's command succeeds instead of failing.

Why this branch rather than the refusal:

- The profile is stored *inside* the Pool document (`agent` / `provider` /
  `model`), and `WorkerPool::load_or_create` already existed as the
  "provision the empty Pool if absent" path that `jjfx pool <N>` itself uses.
  Naming a runtime now calls it first, so there is no new state shape and no
  second provisioning path to keep in step.
- It makes *both* orders work. The dispatch skill's "Before dispatching"
  checklist names the runtime before capacity, and `jjfx pool <N>` after a
  profile still behaves exactly as before - so the documented order needs no
  reordering. That resolves the first bullet of
  `.scratch/dogfood/issues/07-skill-preconditions-and-ordering.md` (the ordering
  complaint); ticket 07 owns any wording change and needs no reorder for this
  item.
- It provisions no capacity: the new document has `size = 0`, no Workers, and no
  Worker Workspace. `jjfx pool <N>` remains the only thing that creates
  capacity, so the existing pool-state lifecycle is unchanged.

**The error can never be the bare `state is missing` again.** The drive's
scenario now prints `Pool profile set to opencode`. If the Pool document is
genuinely absent when the profile is written - the residual case, e.g. it is
removed between the provision and the write - `set_pool_profile` refuses naming
what is missing and the command that creates it:

    cannot load Worker Pool state: cannot set profile Worker Pool: the Worker Pool state does not exist yet; create it with `jjfx pool <N>`

Both branches of the acceptance criteria are implemented and tested; the first
is the shipped behaviour, the second is the residual guard that replaces the
opaque message.

**Tests.**

- `tests/pool_profile.rs` drives the real binary against a real jj Repository
  and writes no Pool state itself:
  - `profile_before_pool_state_is_recorded_without_capacity` - the reported
    failure: `pool profile opencode` succeeds, records `agent = "opencode"`,
    and creates no capacity, no Workers and no Worker Workspace.
  - `a_profile_set_before_the_pool_survives_sizing_it` - profile-then-`pool 1`
    keeps the profile.
  - `setting_a_profile_leaves_existing_capacity_alone` - setting a profile on a
    sized Pool edits the document in place (size, Workers and `created_at`
    untouched).
- The unit test
  `setting_a_profile_without_pool_state_names_the_missing_state` pins the
  residual refusal: it never says `state is missing`, and it names both the
  Worker Pool state and `jjfx pool <N>`.

The first two integration tests were confirmed to fail with the reported
`state is missing` error before the fix landed.

`mise run check` green. Recorded with `momo fix`.
