# jjfx

A keyboard-driven mission-control TUI for running many coding agents in
parallel, each isolated in its own Jujutsu workspace, and shepherding each
workspace's changes from creation to merge.

## Language

### Core

**Workspace**:
An isolated Jujutsu working copy that is the unit of parallel agent work. Each
workspace owns one mutable change chain and hosts at most one agent at a time.
_Avoid_: worktree, checkout, clone

**Default workspace**:
The repository's original workspace and stable home. It is always visible and
cannot be removed through jjfx.

**Agent**:
A supported coding assistant running inside a workspace. jjfx observes agents;
it does not autonomously assign them work.
_Avoid_: worker, bot

**Session**:
One continuous run of an agent inside a workspace. A workspace may host many
sessions over its life, but at most one at a time.

**Attention**:
The single derived, human-facing signal shown per workspace in the list,
collapsing the two lifecycles into "what, if anything, do I need to do here":
needs-you, working, ready-to-forge, or idle.

### Lifecycles

**Agent lifecycle**:
The observed activity state of the agent in a workspace: **Absent** (no known
live session), **Working** (a turn is in progress), **Waiting** (turn finished,
awaiting the human), **NeedsAttention** (blocked on a permission or decision),
or **Ended** (session closed).

**Work lifecycle**:
The least-delivered state among a workspace's owned changes: **Clean** (no
change from trunk), **Dirty** (local change), **Pushed** (bookmark on the
remote), **PrOpen** (PR open, carrying a review verdict), or **Merged**. A local
change makes the workspace Dirty even when a lower change already has a PR.

**Trunk**:
The repository's mainline, which workspace changes are based on and eventually
merged into.

**Change chain**:
The ordered mutable changes owned by one workspace, from its base on trunk to
its working copy.
_Avoid_: branch

**Pull request stack**:
The base-chained pull requests that publish a workspace's bookmarked changes.
When one row represents the stack, it shows the lowest PR carrying the most
blocking verdict: changes requested, review required, no decision, then
approved.
_Avoid_: branch stack

**Forge**:
The pipeline that advances a workspace toward merge: fetch, weld (rebase the
workspace's own mutable chain onto trunk), push, and create or update its pull
request stack. A workspace can be forged on its own or all at once.
_Avoid_: sync, ship, land (for the whole pipeline)

**Weld**:
The forge step that rebases a workspace's change chain onto trunk before it is
pushed.

**Behind**:
How far trunk has advanced past a workspace's base - the drift that
accumulates while a workspace sits idle. Lifting resets it to zero; tidying
workspaces does the same for idle, empty workspaces.

### Maintenance

**Tidy**:
Abandon junk changes - mutable, empty, description-less commits that are not the
working copy, bookmarked, or tagged.

**Tidy workspaces**:
Move every idle workspace (an empty, description-less working copy) onto the
latest trunk, so it starts fresh from the trunk tip instead of drifting behind.
_Avoid_: reset, refresh

**Lift**:
Rebase one workspace's change chain, or all workspace change chains, onto the
latest known trunk without pushing.
