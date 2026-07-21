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
A supported coding assistant running inside a workspace. jjfx observes the
agent lifecycle and may route selected work to it through Workspace Dispatch;
an Agent is not an execution slot.
_Avoid_: bot

**Agent Session**:
The logical interaction between an Agent and a person or dispatch coordinator.
One Agent Session may span several Runs, and a workspace may host many sessions
over its life, but at most one at a time.

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

### Workspace Dispatch

**Worker Pool**:
The Repository-scoped collection of reusable Workers and their execution
capacity.

**Worker**:
A reusable execution slot backed by exactly one Worker Workspace. A Worker is
not an Agent, Agent Session, Workspace, or process.

**Worker Workspace**:
The Workspace assigned to one Worker for its Runs. It is a kind of Workspace,
so it remains visible through the existing Workspace model, but it is not the
Worker itself.

**Worker Status**:
The execution-capacity lifecycle of a Worker, separate from the Agent and Work
lifecycles. It captures whether capacity is available, reserved, occupied by a
Run, or awaiting reconciliation before it can be used again.

**Run**:
One execution attempt by an Agent Runtime in a Worker Workspace. A Run is
shorter-lived than an Agent Session, which may continue across Runs.

**Agent Runtime**:
The external Claude Code or Codex program that executes a Run. The runtime is
not the Agent Session or the Worker that hosts it.

**Ticket**:
A Linear work item selected for implementation. A Ticket can receive a
Reservation and be routed by Dispatch.

**Reservation**:
Execution capacity allocated to a Ticket before its Run starts. A Reservation
prevents competing Dispatch decisions from claiming the same Worker capacity.

**Dispatch**:
The act of routing a Ticket into execution, including the Reservation and Run
lifecycle rules.

**Direct Dispatch**:
A Dispatch that assigns one Ticket directly to one Worker.

**Dispatch Group**:
Dependency-aware progress for a parent Ticket's direct sub-issues. It tracks
which sub-issues are eligible for Dispatch and which remain blocked.

**Dispatch Wave**:
The set of sub-issues in a Dispatch Group whose Dependencies are satisfied and
that may be dispatched concurrently.

**Process**:
An operating-system execution instance. A Process may host an Agent Runtime or
other command, but it is not a Worker, Agent, Agent Session, Run, or Workspace.

Worker, Agent, Agent Session, Workspace, and Process therefore name distinct
things: capacity, assistant, interaction, Jujutsu working copy, and OS
execution instance respectively. A Worker is not itself a Workspace; it is
backed by a Worker Workspace. An Agent Session can continue across Runs, while
a Process may end and be replaced during that interaction.

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
