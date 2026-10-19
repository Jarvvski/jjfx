# `jjfx colocation status` hung once with no output

Status: needs-triage
Type: bug
Blocked by: none

## Problem

During a drive, a read-only state query blocked for more than 60 seconds with no
output and had to be killed; an immediate re-run returned instantly:

    $ jjfx colocation status
    (hung >60s, killed)
    $ jjfx colocation status
    Workspace 'default' is currently colocated with Git.

Not reproduced, and possibly jj lock contention with other jj processes on the
machine at the time (a build and a test run were in flight). Recorded because a
state-reading command that hangs with no output is indistinguishable from a
wedged repository, and the operator's instinct is then to go looking for
corruption.

## Acceptance Criteria

- [ ] Determine whether the hang is reproducible, and under what contention.
- [ ] If it is, either bound the wait with a timeout that names what it is
      waiting on, or make the query lock-free.
- [ ] If it is not reproducible, record the finding and close it.

## References

- `.scratch/bug-hunt/issues/35-load-sensitive-helper-deadlines.md`
- `.scratch/bug-hunt/issues/42-flaky-process-and-exec-tests.md`
