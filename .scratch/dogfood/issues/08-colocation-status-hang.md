# `jjfx colocation status` hung once with no output

Status: resolved
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

- [x] Determine whether the hang is reproducible, and under what contention.
- [x] If it is, either bound the wait with a timeout that names what it is
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

## Resolution (2026-10-20)

**The mechanism survived real contention.** Reproduced, red first, and fixed.

The resource is jj's lock, not CPU, so the reproduction is a second `jj`
holding it. jj-lib's `FileLock` is an exclusive `flock(2)` on
`<root>/.jj/working_copy/working_copy.lock`, taken for the whole of a
working-copy snapshot (confirmed by `lsof` on a snapshotting jj, which also
holds `.jj/repo/git_import_export.lock`). Two independent holds were used:

- a deterministic flock holder on `working_copy.lock`, and
- a **real** `jj st` snapshotting a 60k-file working copy (~4s hold).

Pre-fix, with the lock held, the built binary printed **nothing** and returned
only when the holder released it - the ticket's exact shape:

| run | holder | `jjfx colocation status` |
|---|---|---|
| flock, 8s hold | `working_copy.lock` | **9580ms, 0 bytes until release** |
| real `jj st`, 60k files | snapshot | **10797ms, 0 bytes until release** |
| control, no holder | - | 3453ms |

Also confirmed directly: `jj git colocation status` blocks on *both* locks
(6340ms / 6868ms under a 6s hold), while `jj --ignore-working-copy ...` returns
in 73-94ms against either.

### Fix

`colocation status` is now lock-free rather than bounded. `colocation_command`
(`src/cli.rs`) runs `jj git colocation status --ignore-working-copy` for the
`status` action; `enable`/`disable` are mutations and still snapshot.

The triage's cheaper option - answer from the local `root.join(".git").is_dir()`
check `colocation_flag` already makes - was **not** taken, for two reasons. It
would drop the "Last imported/exported Git HEAD" line an operator reads, and it
is wrong for a secondary workspace: in a colocated repo the *root*'s `.git` is a
directory but a secondary workspace's `.git` is a **file** (a gitdir pointer),
while jj correctly reports that workspace as colocated. `--ignore-working-copy`
keeps jj as the source of truth and produced byte-identical output in every case
tried: colocated, non-colocated, a dirty working copy, and a secondary
workspace. It is also the crate's documented read convention (`src/jj.rs`:
"jjfx must never snapshot the working copy").

Post-fix, with the lock held for 8s, the same binary returns in **176ms**
(`working_copy.lock`) and **164ms** (`git_import_export.lock`), against a
**183ms** uncontended control - indistinguishable from no contention. Against
the real 60k-file snapshot: **357ms**.

A regression test (`colocation_status_does_not_wait_on_the_working_copy_lock`,
`src/cli.rs`) holds the working-copy lock with `rustix::fs::flock` and requires
the read to return within `test_support::LIVENESS_TIMEOUT`. It is not vacuous:
a negative control asserts the same query *without* the flag is still blocked
after 2s, so the lock is demonstrably held. Reverted to the pre-fix code, the
test fails at the liveness budget ("colocation status waited on the
working-copy lock"); with the fix it passes in 2.2s.

### Scope of the class - narrowed, and filed

The triage's guess that "every `jj` invocation in `src/jj.rs`" is affected is
too wide. `read_at_repo`/`read_in_dir` already pass `--ignore-working-copy` and
are lock-free (77-94ms under both holds). What remains:

- Two more production **reads** that omit the flag, both in
  `src/engine/orchestration.rs`: `revision_exists` (5406ms under a 5s hold;
  86ms with the flag) and `resolve_ticket_branch` (5054ms; 77ms with the flag).
  The latter is on the landing path, so the same silent block is reachable from
  a user-visible verb.
- The **mutations** (`run_mut`, `resolve`, `update_stale`, `fix`, `push`,
  `push_bookmark`, and the `crate::jj::command()` callers) must take the lock, so
  waiting is correct - but nothing bounds the wait or names what it is waiting
  on, and that is a deliberate design decision rather than a per-site patch.

Filed as [`12`](12-jj-invocations-have-no-deadline.md) rather than widened into
this fix. Ruled out: `jj config get` (85ms under the same hold - it never
snapshots).

Criterion 3 is left unticked because it is the "not reproducible" branch and the
hang *is* reproducible; it is not applicable rather than unsatisfied.

### Verification

`mise run check`: green. `momo fix` changelog entry added - the operator-visible
change is that `colocation status` no longer snapshots, so it can no longer
block behind another jj process.

