# jjfx

A keyboard-driven mission-control TUI for running many Claude Code agents in
parallel - each isolated in its own Jujutsu workspace - and shepherding each
workspace's changes from creation to merge.

## Language

### Core

**Workspace**:
An isolated Jujutsu working copy that is the unit of parallel agent work. Each
workspace holds one line of change and hosts at most one agent at a time.
_Avoid_: worktree, checkout, clone

**Agent**:
A Claude Code assistant running inside a workspace. jjfx observes agents; it
does not autonomously assign them work.
_Avoid_: worker, bot

**Session**:
One Claude Code run, identified by a stable `session_id` and joined to a
workspace by its `cwd`. A workspace may see many sessions over its life.

**Attention**:
The single derived, human-facing signal shown per workspace in the list,
collapsing the two lifecycles into "what, if anything, do I need to do here":
needs-you, working, ready-to-forge, or idle.

### Lifecycles

**Agent lifecycle**:
What the agent is doing right now, driven by hook events: **Absent** (no live
session) -> **Working** (a turn is in progress) -> **Waiting** (turn finished,
awaiting the human) -> **NeedsAttention** (blocked on a permission or decision)
-> **Ended** (session closed). Working is the interval between a prompt and its
stop; there is no continuous "generating" signal.

**Work lifecycle**:
Where a workspace's change sits on its road to merge: **Clean** (no change from
trunk) -> **Dirty** (uncommitted or committed change) -> **Pushed** (branch on
the remote) -> **PrOpen** (PR open, carrying a review verdict) -> **Merged**.

**Trunk**:
The mainline the work lifecycle targets and the forge rebases onto (jj
`trunk()`, i.e. `main@origin`).

**Forge**:
The pipeline that advances a workspace toward merge: fetch, weld (rebase the
workspace's own mutable chain onto trunk), push, and sync PRs. A workspace can
be forged on its own or all at once.
_Avoid_: sync, ship, land (for the whole pipeline)

**Behind**:
How far `trunk()` has advanced past a workspace's base - the drift that
accumulates while a workspace sits idle. Tidying workspaces resets it to zero.

### Maintenance

**Tidy**:
Abandon junk changes - mutable, empty, description-less commits that are not the
working copy, bookmarked, or tagged.

**Tidy workspaces**:
Park every idle workspace (an empty, description-less working copy) onto the
latest trunk, so idle workspaces start fresh from HEAD instead of drifting
behind.
_Avoid_: reset, refresh
