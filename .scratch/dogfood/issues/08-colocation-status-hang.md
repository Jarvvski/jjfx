# `jjfx colocation status` hung once with no output

Status: ready-for-agent
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

## Triage (2026-10-20)

Triaged from `needs-triage` to `ready-for-agent`. The mechanism is identified by
code inspection - **not reproduced** - and it is a product bug, not only a test
flake.

`colocation status` is not a local read. `colocation_command`
(`src/cli.rs`) runs `jj git colocation status` through `std::process::Command`
with `.status()` and **no deadline**. Any `jj` invocation snapshots the working
copy, which takes the repository's operation-log lock, and jj emits nothing
while it waits. So the observed shape follows directly: a read-only query
blocking silently behind another jj process - the build and test run that were
in flight - and returning instantly once that process finished. The operator
sees no output and no error, which is exactly the "is my repository wedged?"
moment the ticket describes.

**The cheap fix is already half-built.** jjfx already decides colocation locally
as `root.join(".git").is_dir()` in `colocation_flag`
(`src/engine/workspace.rs`), so `status` can answer without invoking `jj` at
all - the ticket's "make the query lock-free" option, rather than bounding a
wait on something that never needed to be asked.

**The mechanism generalises, and the ticket should say whether that is in
scope.** Every `jj` invocation in `src/jj.rs` also carries no deadline, so
`colocation status` is the verb that was *observed* hanging, not necessarily the
only one that can. Confirm or narrow that during the fix and say which it is; if
the class needs its own ticket, file it rather than widening this one silently.

What the worker must do that this triage did not: **confirm the mechanism under
real contention** (AC1). The reproduction is a second `jj` process holding the
lock - the ticket's own reference harnesses (`35`, `42`) show how the load was
generated before.

## References

- `.scratch/bug-hunt/issues/35-load-sensitive-helper-deadlines.md`
- `.scratch/bug-hunt/issues/42-flaky-process-and-exec-tests.md`
