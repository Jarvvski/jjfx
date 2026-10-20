# Every jj invocation jjfx makes carries no deadline

Status: ready-for-agent
Type: bug
Blocked by: none

## Problem

Filed while resolving [`08`](08-colocation-status-hang.md), whose triage asked
whether the hang it found was specific to `colocation status` or a class.

**The class is real, and it is both narrower than the triage feared and wider
than that one verb.** Nothing in jjfx bounds a `jj` invocation. jj prints
nothing while it waits for another jj process to release the repository's
working-copy lock (an exclusive `flock` on
`<root>/.jj/working_copy/working_copy.lock`, taken for the whole of a
working-copy snapshot), so any jjfx command can sit silent indefinitely behind
an unrelated jj process - a build, a test run, another workspace's provisioning.

The triage's "every `jj` invocation in `src/jj.rs`" is **too wide**: the read
helpers `read_at_repo` and `read_in_dir` already pass `--ignore-working-copy`,
which makes them lock-free. It splits in two, and the halves need different
answers.

### 1. Reads that forget `--ignore-working-copy` (a defect, fix on sight)

Measured under a holder of the working-copy lock (5s), the snapshotting form
blocks for the whole hold and the flagged form does not:

| read | plain | with `--ignore-working-copy` |
|---|---|---|
| `jj log -r <rev> --no-graph -T '"ok"' --limit 1` | 5406ms | 86ms |
| `jj bookmark list -T name` | 5054ms | 77ms |
| `jj git colocation status` | 6340ms | 85ms (fixed in `08`) |
| `jj config get user.name` | 85ms | n/a - never snapshots |

Two production reads are still unflagged, both in
`src/engine/orchestration.rs`:

- `revision_exists` - `jj log -r <rev> --no-graph -T '"ok"' --limit 1`.
- `resolve_ticket_branch` - `jj bookmark list -T name`. It is imported into
  `src/cli.rs` and is the single source of truth for "which bookmark carries
  this Ticket's work?", so a user-visible landing verb can block silently the
  same way `colocation status` did.

`08` fixed the third instance, `src/cli.rs::colocation_command`'s `status`.

Ruled out of this half: `jj config get` (never snapshots, 85ms under the hold),
and every mutating helper - that is the next section.

### 2. Mutations that must take the lock, and have no deadline

`run_mut`, `resolve`, `update_stale`, `fix`, `push`, `push_bookmark`, and every
caller of `crate::jj::command()`/`mut_command()` (workspace add/forget, `jj new`,
`jj rebase`, `jj absorb`, `jj commit`, `jj describe`, `jj git fetch`) *must*
snapshot, so waiting for the lock is correct - the work is real and needs it.
What is not correct is that the wait is unbounded and silent. A jjfx verb that
needs the lock cannot tell the operator it is queued, cannot give up, and cannot
say which process holds it. That is a design decision to make deliberately, not
a bug to patch per site.

## Acceptance Criteria

- [ ] The two orchestration reads carry `--ignore-working-copy`, and a test
      holds the working-copy lock and proves they return anyway - the shape
      `08`'s regression test uses (`hold_the_working_copy_lock` in
      `src/cli.rs`).
- [ ] The mutations are decided explicitly, not left by default: either every
      lock-taking jj invocation gets a bounded wait that names what it is
      waiting on (the crate already has `cmd::Cmd::run_with_timeout`), or the
      unbounded wait is a recorded, deliberate choice with its reason.
- [ ] `mise run check` is green.

## Reproducing

Hold the lock, then time the read:

```sh
# any jj process holding the working-copy lock works; this is the deterministic
# one - jj-lib's FileLock is flock(2) on this exact path.
python3 - <<'PY' &
import fcntl, os, time
fd = os.open("<root>/.jj/working_copy/working_copy.lock", os.O_CREAT | os.O_RDWR)
fcntl.flock(fd, fcntl.LOCK_EX); time.sleep(10)
PY
sleep 0.5
time jj --ignore-working-copy log -r @ --no-graph -T '"ok"' --limit 1
time jj log -r @ --no-graph -T '"ok"' --limit 1
```

`.scratch/bug-hunt/load-harness.sh` is for the *flake* family (`35`, `42`, `43`)
and generates CPU/process load, which is **not** this mechanism: the resource
here is jj's lock, so the reproduction is another `jj` (or a flock holder), not
a busy machine. A real jj process works too - a `jj st` over a 60k-file working
copy holds the lock for ~4s.

## References

- [`08`](08-colocation-status-hang.md) - the observed verb, and the fix for it
- `src/jj.rs` (`read_at_repo`, `read_in_dir` - the already-correct read path)
- `src/cmd.rs` (`Cmd::run_with_timeout` - the bounded-run primitive that exists)
- `.scratch/bug-hunt/load-harness.sh` - the other load harness, for the other class
