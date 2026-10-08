# Ubiquitous Language

The canonical vocabulary for jjfx, derived from `CONTEXT.md`, the ADRs, and the
`docs/` references. Terms are grouped by subdomain; bold terms are canonical.

## Core

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Workspace** | An isolated Jujutsu working copy that is the unit of parallel agent work, owning one mutable change chain and hosting at most one agent at a time. | worktree, checkout, clone |
| **Default workspace** | The repository's original workspace and stable home, always visible and never removable through jjfx. | main workspace |
| **Trunk** | The repository's mainline that workspace changes are based on and eventually merged into. | main, master, base branch |
| **Agent** | A supported coding assistant running inside a workspace; jjfx observes it and may route work to it, but it is not an execution slot. | bot |
| **Agent Session** | The logical interaction between an Agent and a person or dispatch coordinator, which may span several Runs. | session |
| **Change chain** | The ordered mutable changes owned by one workspace, from its base on trunk to its working copy. | branch |
| **Pull request stack** | The base-chained pull requests that publish a workspace's bookmarked changes. | branch stack |
| **Behind** | How far trunk has advanced past a workspace's base - the drift accumulated while a workspace sits idle. | staleness |

## Lifecycles

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Agent lifecycle** | The observed activity state of the agent in a workspace: Absent, Working, Waiting, NeedsAttention, or Ended. | agent status |
| **Work lifecycle** | The least-delivered state among a workspace's owned changes: Clean, Dirty, Pushed, PrOpen, or Merged. | work status |
| **Worker Status** | The execution-capacity lifecycle of a Worker: idle (available), busy (occupied by a Run), done, or failed (awaiting Reset). | worker state |
| **Attention** | The single derived human-facing signal per workspace collapsing both lifecycles into needs-you, working, ready-to-forge, or idle. | priority |
| **Review verdict** | The review state carried by an open pull request: approved, review-required, changes-requested, or none. | PR state |

## Forge & publishing

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Forge** | The pipeline that advances a workspace toward merge: fetch, weld, push, and create or update its pull request stack. | sync, ship, land |
| **Weld** | The forge step that rebases a workspace's change chain onto trunk before it is pushed. | rebase (as the step name) |
| **Lift** | Rebase one or all workspace change chains onto the latest known trunk without pushing. | refresh, reset |
| **Bookmark** | A Jujutsu named pointer that is pushed to a remote; the persisted `branch_name` field carries it. | branch |
| **Mount** | Open an interactive terminal session against an existing Worker's Workspace without starting a new Run. | attach |

## Workspace Dispatch

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Workspace Dispatch** | The orchestration layer over Workspaces that owns the Worker Pool, Reservations, Runs, Direct Dispatch, and Dispatch Groups. | scheduler |
| **Worker Pool** | The repository-scoped collection of reusable Workers and their execution capacity. | pool (unqualified) |
| **Worker** | A reusable execution slot backed by exactly one Worker Workspace; not an Agent, Agent Session, Workspace, Run, or process. | agent slot |
| **Worker Workspace** | The Workspace assigned to one Worker for its Runs; a kind of Workspace but not the Worker itself. | worker checkout |
| **Run** | One execution attempt by an Agent Runtime in a Worker Workspace, shorter-lived than an Agent Session. | job |
| **Agent Runtime** | The external Claude Code, Codex, Pi, or OpenCode program that executes a Run. | agent (unqualified) |
| **Ticket** | A Linear work item selected for implementation that can receive a Reservation and be routed by Dispatch. | issue (generic) |
| **Sub-issue** | A direct child Ticket of a parent Ticket within a Dispatch Group. | child ticket |
| **Reservation** | Execution capacity allocated to a Ticket before its Run starts, preventing competing Dispatch decisions from claiming it. | lock |
| **Dispatch** | The act of routing a Ticket into execution, including the Reservation and Run lifecycle rules. | enqueue |
| **Direct Dispatch** | A Dispatch that assigns one Ticket directly to one Worker. | single dispatch |
| **Dispatch Group** | Dependency-aware progress for a parent Ticket's direct Sub-issues, tracking eligibility and blocking. | batch |
| **Dispatch Wave** | The set of Sub-issues in a Dispatch Group whose Dependencies are satisfied and may run concurrently. | batch (for concurrent set) |
| **Follow-up** | A further Run sent to an existing Worker Session, either fresh or resumed. | retry |
| **Reset** | Return a terminal Worker to idle capacity by clearing its Run state and restoring its Workspace to trunk. | clear |

## Maintenance & integration

| Term | Definition | Aliases to avoid |
| --- | --- | --- |
| **Tidy** | Abandon junk changes - mutable, empty, description-less commits not the working copy, bookmarked, or tagged. | prune |
| **Tidy workspaces** | Move every idle workspace (an empty, description-less working copy) onto the latest trunk so it starts fresh from the tip. | reset all, refresh all |
| **Workspace hook** | A repository-local `.jjfx/setup.sh` (transactional provisioning) or `.jjfx/teardown.sh` (best-effort cleanup) script run on every Workspace lifecycle path. | provisioning script |
| **Lifecycle event log** | The shared append-only `events.jsonl` under the state directory that jjfx tails for live Agent transitions. | hooks log |
| **Process** | An operating-system execution instance that may host an Agent Runtime but is not a Worker, Agent, Agent Session, Run, or Workspace. | worker process |

## Relationships

- A **Workspace** owns exactly one mutable **Change chain** and hosts at most one **Agent Session** at a time.
- An **Agent Session** may span several **Runs**, and a single session's process may be replaced between Runs.
- A **Worker Pool** contains zero or more **Workers**, each backed by exactly one **Worker Workspace**, which is itself a kind of **Workspace**.
- A **Ticket** receives at most one **Reservation**, which occupies exactly one **Worker's** capacity for the duration of its **Run**.
- A parent **Ticket's Dispatch Group** tracks many **Sub-issues**, partitioned across successive **Dispatch Waves** by their Dependencies.
- A **Pull request stack** publishes one or more bookmarked changes from a single **Workspace's Change chain**, and each open PR carries one **Review verdict**.

## Example dialogue

> **Dev:** "When I click Forge on a workspace with three changes in its chain, does it create three PRs?"

> **Domain expert:** "It welds the change chain onto trunk first - that's the Weld step - then pushes every bookmark and creates or updates the Pull request stack over them."

> **Dev:** "And if I have three idle workspaces sitting Behind trunk?"

> **Domain expert:** "Tidy workspaces moves each empty working copy onto the trunk tip so they start fresh instead of drifting."

> **Dev:** "What about dispatching AMBA-40 into the Pool? Does it take a Worker immediately?"

> **Domain expert:** "A Reservation claims one Worker's capacity before the Run starts; if AMBA-40 is a parent Ticket with Sub-issues, its Dispatch Group releases them in Dispatch Waves as their Dependencies clear."

> **Dev:** "So a Worker is just another Workspace?"

> **Domain expert:** "No - a Worker is an execution slot backed by exactly one Worker Workspace. It's capacity; the Workspace is the Jujutsu checkout."

## Flagged ambiguities

- **"session" is overloaded.** It names an **Agent Session**, a Pi session header (`pi-sessions`), and an interactive terminal tab ("session tabs"). Use **Agent Session** for the logical interaction; say "terminal tab" or "Pi session header" for the others.
- **"agent" vs "Agent Runtime".** Docs say "the agent running in the pane" but also name Claude/Codex/Pi as runtimes that execute Runs. Use **Agent Runtime** for the external program and reserve **Agent** for the assistant concept.
- **"land" is overloaded.** `CONTEXT.md` explicitly bans "land" as a synonym for the whole Forge pipeline; separately this repo's landing convention uses "land" to mean committing to trunk locally. Never use it for Forge; qualify it if ever needed.
- **"branch" appears everywhere but is avoided.** Prefer **Change chain**, **Bookmark**, and the persisted field name `branch_name`. Do not introduce "branch" as domain language.
- **"pool" alone is ambiguous.** Always qualify as **Worker Pool**. A bare "pool.json" file reference is fine as a persisted-surface name.
- **"worker process" conflates two axes.** A Worker is capacity; a Process is an OS instance. Say which you mean rather than combining them.
- **"sync"/"ship"/"refresh"/"reset" are contested.** Forge supersedes sync/ship; Lift supersedes refresh/reset for rebasing onto trunk; Reset has its own Worker-capacity meaning - keep them distinct.
