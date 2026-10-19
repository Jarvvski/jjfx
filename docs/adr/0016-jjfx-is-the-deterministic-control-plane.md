# jjfx is the deterministic control plane under opencode

Status: accepted

## Context

ADR 0009 adopted Workspace Dispatch as an orchestration layer and gave it a
domain - Worker, Worker Pool, Run, Dispatch - and a seam: a frontend-neutral
engine that owns persistence and execution rules. It decided that jjfx *may*
orchestrate. It did not decide what the orchestration is *for*, so the loop
itself - what is queued, how many times it is retried, what counts as finished,
how much runs at once, and how the sessions relate - stayed wherever it already
was: in prompts, in a skill, and in the operator's head.

A five-line research effort asked how to make a workflow deterministic to run
even though the agents executing it are not (`.scratch/harness/map.md`). Its
answer reframes the tool: **determinism is a property of the harness, not the
model.** Three independent vendors converge on the line. Temporal states it
outright ("your Workflow code must be deterministic; your AI Agent can
absolutely make decisions based on non-deterministic LLM outcomes"); Inngest
draws it the same way; and Anthropic's workflows-versus-agents split puts the
model inside a workflow that is deterministic around it. The model's
non-determinism is confined to *propose* steps; the workflow decides, records,
verifies and replays.

The measurement of what the owner actually runs is why this is urgent rather
than theoretical. In five days: 1,481 agent sessions and $393, of which jjfx's
own Worker Pool accounts for 9 sessions and $0.71. The real topology is
opencode-native subagents in jj workspaces - a driver, one `general` per ticket
in its own workspace, and a read-only, blind `reviewer` - and jjfx is not in
it. Every failure in that data maps onto a pattern the research says prevents
it: 144 reviewer sessions on one pull request, still iterating when its driver
was killed; 693 of 991 reviewers spent in 11 drain sessions; 33 interrupted
sessions whose in-flight children died with them; an instruction of "no more
than 3 at a time" measured at 6; and `resolved` meaning only that the agent said
so. The full survey, its evidence and its ticket breakdown live in
`.scratch/harness/map.md`; this ADR records the decision, not the survey.

## Decision

**jjfx owns the deterministic control plane under the agent runtime: the queue,
the bounds, verification, capacity, and session topology.** The agent runtime is
confined to proposing. jjfx decides what runs, how often it is retried, whether
it actually worked, how much runs at once, and how the sessions are shaped.

This extends ADR 0009 rather than repeating it. ADR 0009 gave the control plane
its domain and its seam; this ADR gives it its *job*. The five responsibilities
above were previously nobody's, and the loop they describe lived nowhere - it
was improvised per session. Where ADR 0009 says jjfx may own orchestration, this
ADR says jjfx owns the determinism of it, using ADR 0009's own domain.

Four decisions, all taken 2026-10-19:

- **Harness, not substrate.** The deterministic loop belongs to the harness -
  jjfx - not to the non-deterministic runtime it drives.
- **Verified done.** A Ticket is done only when a repository-defined check
  actually ran and passed against the change, never on the agent's own report.
  This supersedes `dogfood/06`.
- **One forge path.** "Agents push, open a pull request, merge on green" stays
  the single blessed forge path; `gh stack` and review-bot ingestion are cut.
  Agent-deep means subtraction.
- **The review bound lives in jjfx.** The maximum number of review rounds, the
  required progress and the escalation are enforced by the tool, not by a
  prompt.

## Considered options

The falsified alternatives matter more than the chosen one, because each was
plausible and each was measured or demonstrated false.

- **Determinism at the model** - temperature 0, or grammar-constrained decoding.
  Falsified twice over: temperature 0 is still non-deterministic on every hosted
  API, because kernel batch-variance changes the result run to run, and
  grammar-constrained decoding measurably degrades reasoning. No model setting
  makes the loop reproducible.
- **A prompt-enforced bound.** The owner's own `ticket-batch` skill records the
  lesson from the other side ("14 triggers produced 18 runs: a third of the
  reviewer compute bought nothing"), and the measured "no more than 3 at a
  time" ran 6. The instruction to stop exists and does not work; a bound the
  tool does not enforce is not a bound.
- **The agent's self-report as done.** The 144-session pull request, and a batch
  in which every Ticket read `resolved` with none of the work on trunk, falsify
  it. `resolved` is a claim about the world, so it must be checked against the
  world.
- **Keeping several forge paths** - `gh stack` beside the hand-rolled stack, and
  review-bot ingestion beside the `review` verb. Rejected as the opposite of the
  effort: the point is one blessed path, so the extras are cut rather than
  maintained.
- **Treating a rival orchestrator as the answer.** The competitive sweep found
  the category consolidated in 2026, and it concluded jjfx was the only
  jj-native orchestrator; that conclusion was narrower than it read and is
  corrected in `.scratch/opinionated-loop/map.md`. Even so, no rival makes the
  jj substrate the orchestration model, and none verifies agent output or
  integrates parallel work the way `[checks]` and conflict-as-data do.

## Consequences

- The four decisions stop being open questions for this batch and become the
  premise of its tickets: a durable queue, bounded retries, verified done, a
  capacity governor, and child-session topology (`.scratch/harness/map.md`).
- jjfx's existing domain does most of the work. opencode child sessions are one
  field on a session-create call, and a shared background server means the
  orchestrator already shares a database with every worker. jjfx already
  creates, records, prompts, interrupts and reconciles sessions, and its
  follow-up path already dispatches with no process at all - only the initial
  dispatch still spawns one.
- ADR 0001's "observe, do not orchestrate" stays superseded by ADR 0009, and
  ADR 0003's two orthogonal lifecycles still govern the Agent and Work axes.
  This ADR adds obligations on top of ADR 0009's domain; it does not change it.
- Questions the decision deliberately leaves open are recorded in the map's Fog
  rather than here: whether the durable queue is a jj-native artefact or a
  sidecar, whether jjfx keeps its own Worker Pool or drives sessions directly,
  and how far jj's lack of operation attribution can be worked around for an
  audit trail.
