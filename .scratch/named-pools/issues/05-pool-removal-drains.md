# Removing a pool drains, and reports what it discards

Type: task
Status: ready-for-agent
Blocked by: 04

## Problem

Removing one lane while others keep working raises questions the single-pool world never had to answer, and the failure mode is silent: a Worker's Workspace holds a Change chain, and `Clean` already sweeps every non-default Workspace in one act.

*What happens when a Worker Pool is removed?* settled it:

- **Removal drains.** It refuses while the pool has a busy Worker, naming them - silently killing work in progress is the one outcome nothing undoes. `--force` overrides, and is the only path that loses work.
- **It reports what it will discard before it discards it.** Each Worker's current branch is enumerated with whether it is pushed or merged, and removal refuses if any holds undelivered work unless forced. This matters because of a known gap outside this design: after `Reset`, a Worker's earlier branches are already unreachable from `open-pr` / `rebase` / `absorb`, and removal multiplies that exposure.
- **It does not behave like `Clean`.** `Clean` sweeps the whole Repository; removing a pool touches only its own Workers' Workspaces. That is what makes lane isolation real.
- **Those Workspaces are removed, not left behind** - leaving them would be a slower, quieter version of the same leak, since nothing would ever come back for them.

## Acceptance Criteria

- [ ] Removal refuses while any Worker in the pool is busy, naming them.
- [ ] Removal enumerates each Worker's branch and whether it is pushed or merged, and refuses on undelivered work unless forced.
- [ ] Removal touches only the removed pool's Workers' Workspaces, leaving other pools running.
- [ ] `--force` is the only path that discards work, and says so.
- [ ] `mise run check` is green.

## References

- `src/cli.rs` (`destroy_pool_command`), `src/engine/workspace.rs` (the clean/removal path)
- `.scratch/multi-pool/issues/10-what-removing-a-pool-does.md`
