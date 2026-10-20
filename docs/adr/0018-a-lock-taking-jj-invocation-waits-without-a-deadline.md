# A lock-taking jj invocation waits without a deadline

Status: accepted

## Context

jjfx spawns jj for two kinds of work, and the difference decides how it must
handle jj's working-copy lock.

A **read** can be made lock-free. `--ignore-working-copy` tells jj to answer
from the operation log without snapshotting the working copy, so the read never
takes the lock. jjfx's read helpers (`jj::read_at_repo`, `jj::read_in_dir`)
already carry the flag, dogfood 08 made `colocation status` lock-free, and
dogfood 12 fixed the last two reads that had forgotten it.

A **mutation** cannot. Snapshotting *is* the mutation: `jj commit`, `jj
describe`, `jj new`, `jj rebase`, `jj absorb`, `jj workspace add`, `jj git
fetch`, `jj resolve`, `jj op restore`, `jj fix` and the rest must take the
working-copy lock - an exclusive `flock(2)` on
`<root>/.jj/working_copy/working_copy.lock`, held for the whole of a snapshot.
Waiting for it is not a defect; it is the price of the work.

The defect is that the wait is unbounded *and silent*. jj prints nothing while
it waits, so a jjfx verb that needs the lock cannot tell the operator it is
queued, cannot give up, and cannot name the process holding it. That is the
shape dogfood 08 observed and mistook for a wedged repository, and dogfood 12
asks for it to be decided rather than left by default.

## Decision

**A lock-taking jj invocation waits for the lock without a deadline. The
unbounded wait is deliberate: jjfx does not kill a mutation because it is
taking too long.**

The decision is recorded at the seam every mutation routes through -
`jj::command` and `jj::mut_command` in `src/jj.rs` - as well as here.

## Considered options

- **Bound every mutation with `Cmd::run_with_timeout`.** Rejected. The bound
  works by killing the child, and jjfx's mutation callers read a killed run as
  "the mutation did not happen". For a mutation that is killed part-way - a `jj
  workspace add` before its working copy is written, a `jj git push` after the
  remote accepted it, a `jj op restore` part-way through - that reading is
  wrong, and jjfx's recorded state diverges from the repository, or a retry
  double-applies. jj's own op-log transaction is crash-consistent, but the
  remote side of a push is not, and a timeout cannot be told from a genuine
  failure at the call site. A bound long enough not to fire on legitimate work
  (a fetch over a slow network, a rebase of a long chain) is also not a bound on
  the *lock wait* at all - it is a bound on the whole operation, which this
  crate already refuses deliberately (`src/cmd.rs`: `run_with_timeout` is "a
  separate method rather than a bound on `run`: agent Runs and `jj` operations
  are legitimately long").
- **A pre-flight probe that detects a held lock and names the holder.** Rejected
  for now. jjfx would have to probe `<root>/.jj/working_copy/working_copy.lock`
  with a non-blocking `flock` before spawning jj, which is racy - the lock can
  be taken between the probe and jj's own attempt - and would only ever produce
  a hint, not a bound. jj exposes no lock-wait timeout and no "waiting for lock"
  message to hook, so the honest form is a notice that may be wrong. Worth
  revisiting if jj grows either signal; it is not worth a racy guess today.
- **Leave it silent, with no recorded reason.** Rejected. This is what dogfood
  12 exists to end: an unbounded wait nobody chose is indistinguishable from a
  bug the next time a verb hangs.

## Consequences

- Reads stay lock-free, and a read that forgets `--ignore-working-copy` is a
  defect to fix on sight. The class is now closed: every production read goes
  through `read_at_repo`/`read_in_dir`.
- Mutations wait, and the wait is a recorded choice with its reason rather than
  a default. A verb that blocks is doing real work, not stuck.
- `Cmd::run_with_timeout` stays for the terminal query, the one wait that must
  be bounded, and is not applied to jj.
- The gap this leaves - an operator who cannot tell a queued mutation from a
  working one - is known and accepted. `lsof`/`fuser` on the lock file names the
  holder today; a jj-native signal is the thing that would close it.
