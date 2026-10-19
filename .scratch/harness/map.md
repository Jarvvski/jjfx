# The harness: jjfx as the deterministic control plane under opencode

Status: open (filed 2026-10-19)

## Why this exists

A five-line research effort asked how to build a workflow that is deterministic
to run even though the agents executing it are not - and what the owner
actually runs. The answer reframed the tool.

**Determinism is a property of the harness, not the model.** Three independent
vendors converge: Temporal ("your Workflow code must be deterministic; your AI
Agent can absolutely make decisions based on non-deterministic LLM outcomes"),
Inngest, and Anthropic's workflows-versus-agents split. Model-level determinism
is a dead end - temperature 0 is still non-deterministic on every hosted API
because of batch-variance in kernels - and grammar-constrained decoding
measurably degrades reasoning. The model's non-determinism is confined to
"propose" steps; the workflow decides, records, verifies and replays.

**The owner's real loop does not include jjfx.** In five days: 1,481 sessions,
$393, of which jjfx's own Worker pool is 9 sessions and $0.71. The real topology
is opencode-native subagents in jj workspaces - `build` -> `general` (one per
ticket, in its own workspace) -> `reviewer` (read-only, blind) - with 976
`general -> reviewer` edges.

**And the pain is exactly where the harness is missing.** Every failure in that
data maps onto a pattern the research says prevents it.

| Observed | Pattern that prevents it |
|---|---|
| 144 reviewer sessions on one PR, passes to 18, still iterating when killed; 693 of 991 reviewers in 11 drain sessions | bounded retries with escalation |
| 33 interrupted sessions whose in-flight children died with them | checkpoints and resumption |
| 54 `(fork #N)` sessions, one title x33 | compaction and checkpointing |
| the 7 review dimensions recur 8-11x | idempotency keyed on step identity |
| told "no more than 3 at a time", measured 6 | a deterministic capacity governor |
| `resolved` means the agent said so | verify the outcome, not the self-report |

## Decisions

- **Harness, not substrate (2026-10-19).** jjfx owns the deterministic control
  plane: the queue, the bounds, verification, capacity and session topology.
  Today the loop lives nowhere - it is improvised per session.
- **Verified done (2026-10-19).** A Ticket is done only when a repo-defined
  check actually ran and passed against the change, never on the agent's own
  report. This supersedes `dogfood/06`.
- **One forge path (2026-10-19).** Keep "agents push, open a PR, merge on
  green" as the single blessed forge path; cut `gh stack` and review-bot
  ingestion. Agent-deep means subtraction.
- **The review bound lives in jjfx (2026-10-19).** Tool-enforced, not
  prompt-enforced.

These four are recorded as **ADR 0016**
(`docs/adr/0016-jjfx-is-the-deterministic-control-plane.md`), which extends
ADR 0009: 0009 gave the control plane its domain and its seam, 0016 gives it
its job.

## What already exists (so this is cheaper than it looks)

- opencode **child sessions are one field** - `POST /api/session {parentID,
  agent, model, location, permissions}` - and the `subagent` tool is not the
  only route. A shared background server means the orchestrator already shares
  a database with every worker.
- jjfx already creates, records, prompts, interrupts and reconciles opencode
  sessions, and its **follow-up path already dispatches with no process at
  all**. Only the initial dispatch still spawns one.
- Ticket 18's `[checks]` gate already runs a build check between weld and push,
  so verified-done is mostly wiring the status to it.

## What does not exist

- **No durable queue.** `waiting.json` is one dispatch's held-back set; the
  dispatch group is a progress tracker, not a work queue.
- **No child-session observability.** The opencode plugin emits no subagent
  events; the fold keys by `cwd` and tracks at most one session per workspace.
- **The forge is inside the dispatch prompt** (`dispatch_prompt.rs:150-237`),
  so shrinking it is a prompt change, not a deletion.

## Corrections to the record

`opinionated-loop/map.md` claims every rival is git-worktree-based and that jjfx
is the only jj-native orchestrator. Both are false: container-based isolators
exist (container-use, Sculptor, Terragon), and there is a jj-native cluster
(jj-navi, ao-jj, jjx, agentic-jujutsu, Kaisen, smithers-orchestrator). What is
true is narrower and stronger - **jjfx is the only tool that makes the jj
substrate the orchestration model**. The field's two unsolved problems are
verifying agent output and integrating parallel work, which is what jjfx's
`[checks]` and conflict-as-data already address. Applied to
`opinionated-loop/map.md` on 2026-10-19.

## Tickets

### Phase 0 - the trust boundary

- [01](issues/01-record-the-decision.md) - docs: the ADR and the map correction.
- [02](issues/02-verified-done.md) - task: a Ticket is done only when a check
  ran and passed, never on the agent's self-report. Supersedes `dogfood/06`.

### Phase 1 - the loop

- [03](issues/03-dispatch-without-a-process.md) - task: reuse the follow-up
  session seam for the initial dispatch. The smallest step to agent-deep.
- [04](issues/04-bound-the-review-loop.md) - task: a tool-enforced bound on the
  review loop: max rounds, findings must decrease, escalate on stall.
- [05](issues/05-capacity-governor.md) - task: a deterministic concurrency
  limit, enforced by the tool rather than the prompt.
- [06](issues/06-durable-queue.md) - task: a durable work queue. Blocks 07, 09.
- [07](issues/07-checkpoint-and-resume.md) - task: an interruption must not
  orphan in-flight work. Blocked by 06.
- [09](issues/09-idempotent-steps.md) - task: steps keyed by identity, so a
  re-run does not duplicate work. Blocked by 06.

### Phase 2 - topology

- [08](issues/08-child-session-topology.md) - task: workers as child sessions,
  with the plugin and fold modelling them.

- [11](issues/11-declare-jjfxs-own-check.md) - task: declare this
  repository's own check, so the loop can verify work in the repo that builds it.

### Phase 3 - subtraction

- [10](issues/10-cut-the-forge-to-one-path.md) - task: one blessed push+PR
  path; cut `gh stack` and review-bot ingestion; take the forge out of the
  prompt.

## Fog

- Whether `jjfx` should keep its own Worker pool at all, or drive opencode
  sessions directly with the pool as a capacity declaration.
- Whether jj's lack of operation attribution (no tool/process tag) can be
  worked around for an audit trail, or must be accepted.

The durable queue's jj-native-versus-sidecar question is settled: it is a
sidecar (ADR 0017, ticket 06).
