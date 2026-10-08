# jjfx

A keyboard-driven mission-control TUI for running many coding agents in
parallel, each isolated in its own Jujutsu workspace, and shepherding each
workspace's changes from creation to merge.

## Language

### Core

**Repository**:
A single Jujutsu repository that jjfx operates on: the container for its
Workspaces and Trunk and the scope of its Worker Pool.
_Avoid_: repo (in prose), project

**Workspace**:
An isolated Jujutsu working copy that is the unit of parallel agent work. Each
workspace owns one mutable change chain and hosts at most one agent at a time.
_Avoid_: worktree, checkout, clone

**Default workspace**:
The repository's original workspace and stable home. It is always visible and
cannot be removed through jjfx.
_Avoid_: main workspace

**Ad Hoc Workspace**:
A Workspace a person creates directly through the CLI or TUI for manual work;
it is not backed by a Worker.
_Avoid_: manual workspace

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
remote), **PrOpen** (PR open, carrying a Review verdict), or **Merged**. A local
change makes the workspace Dirty even when a lower change already has a PR.
**Unknown** records that jj or gh could not determine the state.

**Review verdict**:
The review state carried by an open pull request: approved, review-required,
changes-requested, or none.
_Avoid_: PR state

### Change & publishing

**Trunk**:
The repository's mainline, which workspace changes are based on and eventually
merged into.

**Change chain**:
The ordered mutable changes owned by one workspace, from its base on trunk to
its working copy.
_Avoid_: branch

**Bookmark**:
A Jujutsu named pointer that can be pushed to a remote; the persisted
`branch_name` field carries it.
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
How far trunk has advanced past a workspace's base - the drift that accumulates
while a workspace sits idle. Lifting resets it to zero; tidying workspaces does
the same for idle, empty workspaces.

### Workspace Dispatch

**Worker Pool**:
The Repository-scoped collection of reusable Workers and their execution
capacity.

**Worker**:
A reusable execution slot backed by exactly one Worker Workspace. A Worker is
not an Agent, Agent Session, Workspace, or process.
_Avoid_: agent slot

**Worker Workspace**:
The Workspace assigned to one Worker for its Runs. It is a kind of Workspace,
so it remains visible through the existing Workspace model, but it is not the
Worker itself.
_Avoid_: worker checkout

**Worker Status**:
The execution-capacity lifecycle of a Worker: **idle** (available), **busy**
(occupied by a Run), **done**, or **failed**. Both terminal states await Reset
before capacity is available again. Separate from the Agent and Work lifecycles.
_Avoid_: worker state

**Run**:
One execution attempt by an Agent Runtime in a Worker Workspace. A Run is
shorter-lived than an Agent Session, which may continue across Runs.

**Run result**:
The structured completion of a Run: whether it succeeded or failed, plus usage,
duration, turns, and cost when reported.
_Avoid_: outcome

**Agent Runtime**:
The external Claude Code, Codex, Pi, or OpenCode program that executes a Run.
The runtime is not the Agent Session or the Worker that hosts it.
_Avoid_: agent (unqualified)

**Agent Runtime Profile**:
The provider-and-model selection that configures how an Agent Runtime launches a
Run.
_Avoid_: profile (unqualified)

**Ticket**:
A Linear work item selected for implementation. A Ticket can receive a
Reservation and be routed by Dispatch.
_Avoid_: issue (generic)

**Sub-issue**:
A direct child Ticket of a parent Ticket within a Dispatch Group.
_Avoid_: child ticket

**Dependency**:
A Sub-issue that must be completed before another Sub-issue becomes eligible for
Dispatch.
_Avoid_: blocker

**Reservation**:
Execution capacity allocated to a Ticket before its Run starts. A Reservation
prevents competing Dispatch decisions from claiming the same Worker capacity.
_Avoid_: lock

**Dispatch**:
The act of routing a Ticket into execution, including the Reservation and Run
lifecycle rules.
_Avoid_: enqueue

**Direct Dispatch**:
A Dispatch that assigns one Ticket directly to one Worker.
_Avoid_: single dispatch

**Dispatch Group**:
Dependency-aware progress for a parent Ticket's direct Sub-issues. It tracks
which Sub-issues are eligible for Dispatch and which remain blocked.
_Avoid_: batch

**Dispatch Wave**:
The set of Sub-issues in a Dispatch Group whose Dependencies are satisfied and
that may be dispatched concurrently.
_Avoid_: batch (for concurrent set)

**Follow-up**:
A further Run dispatched to a Worker after its previous Run ended, either
resuming the prior Agent Session or starting a fresh one.
_Avoid_: retry

**Reset**:
Returning a terminal Worker to idle capacity by abandoning its Run and restoring
its Workspace to trunk. Distinct from Lift and Tidy workspaces: Reset clears
abandoned Run state on a Worker; those rebase onto trunk without touching Run
state.
_Avoid_: clear

**Mount**:
Opening an interactive terminal session against an existing Worker's Workspace
without starting a new Run.
_Avoid_: attach

**Dismiss**:
Removing an idle Worker from the Pool; for a terminal Worker it instead performs
an in-place Reset.
_Avoid_: remove (unqualified)

Worker, Agent, Agent Session, Workspace, and Process therefore name distinct
things: capacity, assistant, interaction, Jujutsu working copy, and OS execution
instance respectively. A Worker is not itself a Workspace; it is backed by a
Worker Workspace. An Agent Session can continue across Runs, while a Process may
end and be replaced during that interaction.

### Maintenance & integration

**Tidy**:
Abandon junk changes - mutable, empty, description-less commits that are not the
working copy, bookmarked, or tagged.
_Avoid_: prune

**Tidy workspaces**:
Move every idle workspace (an empty, description-less working copy) onto the
latest trunk so it starts fresh from the trunk tip instead of drifting behind.
_Avoid_: refresh all

**Lift**:
Rebase one workspace's change chain, or all workspace change chains, onto the
latest known trunk without pushing.
_Avoid_: refresh

**Clean**:
Remove every non-default Workspace from the Repository in one sweep.
_Avoid_: purge

**Workspace hook**:
A repository-local `.jjfx/setup.sh` (transactional provisioning) or
`.jjfx/teardown.sh` (best-effort cleanup) script run on every Workspace lifecycle
path.
_Avoid_: provisioning script

**Lifecycle event log**:
The shared append-only `events.jsonl` under the state directory that jjfx tails
for live Agent transitions.
_Avoid_: hooks log
