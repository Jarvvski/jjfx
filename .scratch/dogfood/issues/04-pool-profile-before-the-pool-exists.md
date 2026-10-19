# `jjfx pool profile` should work before the pool exists

Status: ready-for-agent
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

- [ ] `jjfx pool profile <name>` works on a repository with no pool state yet
      (provisioning what it needs), or
- [ ] it refuses with a message naming the missing state and the command that
      creates it.
- [ ] The skill's ordering matches whichever behaviour is chosen - see
      `.scratch/dogfood/issues/07-skill-preconditions-and-ordering.md`.

## References

- `skills/jjfx-dispatch/SKILL.md` ("Before dispatching")
