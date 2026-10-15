# Opinionated loop: dispatch, review/landing, and trust

Status: open (filed 2026-10-15)

## Notes

Scope: jjfx at 0.54.0. This effort is the output of a strategy review
(2026-10-15) whose question was "what features should we add, and how do we make
this an opinionated tool?" The owner anchored it to **his own workflow**:
opencode as the day-to-day runtime, Linear + GitHub as tracker/forge, local
`.scratch/` markdown tickets for OSS/personal repos, driven from the TUI, the
CLI, and an agent via the bundled skill. His three named frictions were the
**dispatch loop**, **review & landing**, and **trustworthiness**.

The review's competitive sweep (Conductor, Sculptor, Crystal/Nimbalyst,
vibe-kanban, claude-squad, Terragon, Superset, Paseo, container-use) found jjfx
is the **only jj-native orchestrator** — every rival is git-worktree-based — and
that the category consolidated in 2026 (Terragon shut down; Crystal became
Nimbalyst; vibe-kanban sunsetting). The strategic call is therefore **not**
feature-parity with cloud/GUI apps; it is to lean into jj-native + terminal +
one opinionated path.

**Opinionation means subtraction.** jjfx is already opinionated (attention-first,
forge pipeline, two orthogonal lifecycles). "More opinionated" here means:
curate toward opencode + Linear/GitHub + local markdown, retire the paths that
serve nobody (the Pi discovery helper — already tracked by
[`remove-pi`](../remove-pi/issues/01-remove-pi.md)), and collapse the loop to one
blessed path per axis.

Method: same as `bug-hunt` — verification-first where a claim is load-bearing
(confirm against `jj`/`gh`/the vendor before building), one focused commit per
`## Commits` item, `mise run check` green, a dated `CHANGELOG.md` entry via momo,
and **land locally only** (never push — `CLAUDE.md`).

Confidence key, where a ticket carries claims:

- **verified** — read in source or confirmed against the tool.
- **sweep** — from the external survey; needs a spike to confirm before building.
- **decision** — a product call the owner must make; not agent-decidable.

## The bet

**Kill the Pi↔Linear coupling.** Ticket discovery is hardwired to Pi today:
`PI_DISCOVERY_HELPER_ENV = "JJFX_PI_LINEAR_HELPER"` (`src/engine/ticket.rs:19`)
names a Pi-only helper, so on any other runtime `dispatch --all` does not work —
you name ticket ids by hand. The owner runs opencode, and uses local `.scratch/`
tickets for OSS/personal repos, so on those repos the pool cannot pull work at
all. The seam already exists: `TicketQuery` (`:511`) is a trait,
`AgentRuntimeQuery` (`:202`) implements it per-runtime, and `TicketDiscovery`
(`:699`) is generic over it. A runtime-independent ticket source drops straight
in. This is the effort's foundation and its highest-leverage change.

## Tickets

### Phase 0 — foundation (the bet)

- [01](issues/01-provider-neutral-ticket-source.md) — task: make the ticket
  source a configured, runtime-independent choice behind the existing
  `TicketQuery` seam. Blocks 02.
- [02](issues/02-local-markdown-tickets.md) — task: a first-class local
  `.scratch/` markdown ticket source (the bet). Blocked by 01.

### Phase 1 — dispatch loop

- [03](issues/03-lifecycle-signals.md) — task: ingest the agent hook events jjfx
  ignores (needs-input, quota-blocked, subagent) into the shared lifecycle.
- [04](issues/04-opencode-api-send-permission.md) — task: API-native `send`,
  follow-up, and permission resolution for opencode.
- [05](issues/05-headless-parity.md) — task: drive runs without a terminal so
  dispatch stops failing closed. Large; needs a design decision.
- [06](issues/06-attention-lane-and-glyphs.md) — task: the attention-first lane
  and per-state glyph vocabulary.
- [16](issues/16-durable-run-cost-record.md) — task: persist the per-Run cost and
  usage jjfx already parses but discards at finalization. **Blocks 07** — the
  costs 07 aggregates are not currently retained anywhere.
- [07](issues/07-pool-cost-budget-view.md) — task: a pool-level cost/usage view
  and a dispatch gate on a spend cap. Blocked by 16.

### Phase 2 — review & landing

- [08](issues/08-review-bot-ingestion.md) — task: ingest an AI reviewer's
  findings and route them through the existing `review` verb.
- [17](issues/17-migration-order-check.md) — task: flag migration-order
  inversions across open workspaces before either lands. A set property no
  per-PR check can see.
- [09](issues/09-gh-stack-adoption.md) — task: adopt `gh stack` for stack state
  instead of hand-rolling base-chaining. Spike first.
- [10](issues/10-opinionated-landing.md) — decision: an opt-in landing rule
  (auto-merge on approved+green, or a merge-queue handoff). **resolved** —
  option (a), auto-merge on green, ruled by the owner 2026-10-15.
- [11](issues/11-jj-fix-pre-push.md) — task: run `jj fix` as a forge step.
- [18](issues/18-forge-build-check.md) — task: verify a change builds against
  merged trunk before the forge pushes it. The missing half of the lifecycle -
  today `done` means only that the agent said so.

### Phase 3 — trustworthiness

- [12](issues/12-oplog-undo.md) — task: expose jj's operation log as an undo
  surface. Cheap, high-value.
- [13](issues/13-conflicts-first-class.md) — task: promote conflicts to a
  first-class Work/Attention state with resolve.
- [14](issues/14-stuck-watchdog-capacity-governor.md) — task: stuck-agent
  detection and a dispatch capacity governor.
- [15](issues/15-jj-absorb-review-edits.md) — task: use `jj absorb` for
  review-feedback edits.

## Decisions so far

- **Landing rule (2026-10-15).** The owner ruled that for this batch agents
  push, open a PR per ticket, and merge once the PR's checks are green, then
  take the next ticket. Ticket 10 is resolved as option (a). The owner also
  **overrode** the repo `CLAUDE.md` "agents must never push from this
  repository" rule for this batch only — the standing rule is otherwise in
  force. `main` is unprotected (no required checks, no rulesets), so the gate is
  the PR's own CI `check` job (`mise run check`).
- The competitive frame is a **personal-tool** frame, not an adoption-growth
  frame: the owner's answer was "sharpen it for my own workflow". Public-growth
  strategy is out of scope for this effort.
- "Opinionated stack" is read as **curate toward the owner's path**
  (opencode-first; Linear + GitHub + local markdown as the two blessed ticket
  sources; retire Pi), **not** remove claude/codex. If the harder line
  (opencode-only) is wanted, that is a separate decision.
- The Pi removal itself is **already tracked** by
  [`remove-pi`](../remove-pi/issues/01-remove-pi.md) (`ready-for-agent`). This
  effort does not duplicate it; 01/02 supersede the *discovery* half of Pi's
  purpose (the `JJFX_PI_LINEAR_HELPER` protocol), which `remove-pi` deletes.
  Sequencing: 01 and 02 can land before or after `remove-pi`; if `remove-pi`
  lands first, 01's "keep the shared abstraction" note is already satisfied.

## Fog

- Does the field's "attention lane" converge far enough to standardise a
  vocabulary, or is a per-tool glyph set fine? Parked until 06.
- Whether `gh stack link` fully replaces jjfx's own stack assembly without
  behavioural loss is unverified — 09 is a spike for exactly this.
- Headless parity (05) may not be worth it for a single-user local tool; it is
  filed because it removes the `--force` ambiguity, but it may become `wontfix`.

## Comments

- 2026-10-15: Filed from the strategy review.
- 2026-10-15: **16, 17 and 18 added**, from the evidence of a 24-ticket batch (15
  merged PRs) run through parallel agents in another repo. The batch's cost
  analysis found three things this effort can act on: **65% of the spend was the
  review fan-out** (evidencing 08's bounded loop with a pass cap rather than a
  findings count), the blockers that actually cost the merges were **cross-PR and
  invisible to every gate** (17, 18), and the per-Run costs **07** aggregates were
  never retained (16, which now blocks it). It also found one jjfx premise wrong:
  "one editor per workspace" is already structurally impossible, since a Worker is
  1:1 with its own workspace by construction - so no lease is needed.
