# A Worker is never told its commit has diverged or is no longer reachable

Status: resolved
Type: task
Blocked by: none

## Problem

jj's operation log is deliberately lock-free: concurrent `jj` processes each see a consistent view, write their own operation, and jj reconciles divergent operations on the next command. A concurrent operation can therefore leave a Worker's commit in one of two states jj calls a *divergence*:

- **divergent** - two commits share one change id (e.g. a repository-wide `jj undo` restored a Workspace's working-copy commit while that Workspace's own `jj commit` rewrote it), or
- **hidden** - the commit was rewritten or restored out of the visible set, so `jj log` no longer shows it even though the object survives in the op log.

jjfx reads the operation log (`src/jj.rs::operation_log` / `operation_id`) and drives `jj op restore` for undo (`src/engine/undo.rs`), but **nothing in jjfx reads jj's `divergent()` revset or otherwise checks whether a Workspace's commit is still reachable**. A Worker whose commit diverged or went hidden is never told; the only thing that catches it is the Worker reading its own history carefully.

That is exactly what happened in the `dogfood/13` sighting behind ticket 44: the Worker noticed a divergent head by hand and re-applied its edit, so nothing was lost *that time*. Ticket 44 removed jjfx's own path to causing that divergence (a failed-push rollback no longer passes `jj undo --allow-cross-workspace`), but it did not add the capability to *tell* a Worker - and a divergence can still be caused by anything else running against the shared log (another agent's `jj`, a repository-wide `jj op restore`, an owner's `jj` in another Workspace).

## Why this is worth a ticket rather than prose

The batch already recorded that **jj attributes an operation to no tool and no process**, so after the fact there is no way to say which agent's operation diverged the log. The one thing jjfx *can* do is read the result: jj exposes divergence as structure (the `divergent()` revset, the `??` marker in `jj log`), and jjfx owns the Workspace model, so it can say "Workspace `worker-...`'s commit is now divergent" rather than leaving the Worker to discover it.

## Acceptance Criteria

- [x] jjfx reads whether a Workspace's commit has diverged or become unreachable from the visible set, from a machine-readable jj source (jj's `divergent()` revset, not jj's English prose), and surfaces it - a `status`/`undo` field or a verb - so a Worker is told rather than having to read its own history.
- [x] The surface distinguishes "this commit diverged" from "this commit is gone" (hidden or abandoned), or says plainly which it cannot tell apart. (It says plainly: it can tell diverged, not gone.)
- [x] A test drives the state (a commit made divergent by a concurrent operation in a throwaway repository) and asserts jjfx reports it; hermetic per `AGENTS.md`'s "Test hermeticity (CI parity)".
- [x] If it cannot be done from a stable jj source, say so and close with what was ruled out. (It can be done from `divergent()`; "gone" cannot, and is stated.)

## References

- `.scratch/bug-hunt/issues/44-concurrent-operations-diverge-the-op-log.md` (the reproduction and the rollback fix)
- `.scratch/dogfood/issues/13-open-pr-docs-contradict-the-tool.md` (the sighting)
- `src/jj.rs` (`operation_log`, `operation_id`, `restore_operation`)
- `src/engine/undo.rs` (`UndoOutcome::Refused { later: None }` is the closest existing "cannot tell" surface, and it is about the undo record, not a commit)

## Resolution (Thu 22 Oct 2026)

**Resolved - jjfx reads divergence from jj's `divergent()` revset and surfaces it on `jjfx status`.**

### The surface

`jjfx status` (and `status --json`, which `pool list` shares) now reports a Worker
whose Workspace commit has diverged:

- the human Worker row gains a `diverged:` line naming the change id, how many
  visible commits share it, and the revset that shows them
  (`jj log -r 'divergent()'`);
- `status --json` gains a top-level `diverged` array, one entry per diverged
  Worker: `{"worker", "pool", "change_id", "copies"}`.

`status` is the surface a Worker already reads to learn about itself, and the
Workspace model already maps a Worker to its jj Workspace (the Workspace is named
for the Worker), so no new verb was needed.

### What was read, and what it can and cannot tell apart

The read is `src/jj.rs::workspace_divergence`, from jj's **`divergent()` revset**
intersected with the Workspace's own chain
(`((trunk).."<ws>"@) & divergent()`, the same `trunk..<ws>@` chain
`conflicted_commits` walks), plus `divergent() & change_id(<id>)` under jj's own
`--count` for the number of copies. Both are machine-readable; jj's English log
is never read.

- **Can tell: a commit diverged.** `divergent()` marks a change id with more than
  one *visible* commit - exactly the state a concurrent operation leaves (two
  commits, one change id). Verified against jj 0.46.0.
- **Cannot tell: a commit is gone.** A commit rewritten or restored out of the
  visible set reads as intact, because jj keeps every Workspace's `@` on a
  *visible* commit - it creates a fresh one when the recorded commit is abandoned
  - so a gone commit and one that never existed look identical from jj's visible
  state. jjfx records no identity for the Workspace's commit to compare against,
  so it cannot tell them apart. That is stated in `WorkspaceDivergence::Intact`'s
  docs and pinned by the test
  `a_commit_rewritten_out_of_the_visible_set_reads_as_intact`: the abandoned
  change is genuinely absent from `all()`, yet the Workspace reads as intact. This
  is the "says plainly which it cannot tell apart" outcome the second criterion
  allows.

A jj failure is a typed error, never a silent `Intact`: "not divergent" and "jj
could not be read" do not look alike. `status` skips a Worker whose read fails (a
Worker with no Workspace yet is ordinary), so one broken read cannot fail the
whole command.

### How the test drives a real divergence (hermetic)

`test_support::diverge_working_copy` drives a **real** divergence
deterministically, not by racing: two operations are derived from the *same*
starting operation (`jj --at-operation`) and each rewrites the Workspace's
working-copy commit (one change id, two commit ids), then both are integrated
(`jj op integrate`). jj reconciles them into a divergent change - the shape the
`undo --allow-cross-workspace` race in ticket 44 produced - without the race's
flakiness.

- `jj::tests::a_divergent_workspace_commit_is_read_from_the_divergent_revset`
  asserts the read reports `Divergent { copies: 2 }`;
- `jj::tests::an_undiverged_workspace_reads_as_intact` and
  `jj::tests::a_commit_rewritten_out_of_the_visible_set_reads_as_intact` pin the
  clean case and the documented limit;
- `cli::tests::status_reports_a_worker_whose_workspace_commit_diverged` builds a
  real repository with a `worker-01` Workspace, diverges it, and asserts `status`
  reports it - the end-to-end proof the surface is wired;
- `status_json_carries_a_diverged_workspace_commit` and
  `status_json_diverged_is_empty_without_a_divergence` pin the JSON shape.

Every test owns its jj config (the throwaway repositories come from
`store::test_local_repo`/`temp_repo`, which write a repo-local identity), so they
pass on a clean-config runner. Proved both ways: the five new tests pass with the
developer's jj config **and** with `JJ_CONFIG` pointed at an empty file and
`HOME`/`XDG_CONFIG_HOME` redirected.

### Files

- `src/jj.rs` - `WorkspaceDivergence` and `workspace_divergence` (the read).
- `src/cli.rs` - the `diverged` array on `PoolReport`, the Worker-row line, and
  the `diverged` JSON array.
- `src/test_support.rs` - `diverge_working_copy`, the one divergence recipe.
- `CHANGELOG.md` - a `momo add` entry.

### Gate

`mise run check` - green (changelog + fmt + clippy + build + full test suite).
