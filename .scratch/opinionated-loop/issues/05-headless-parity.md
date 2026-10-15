# Drive runs without a terminal (remove the `--force` ambiguity)

Status: wontfix
Type: task
Blocked by: none

## Problem

jjfx decides whether a Workspace is open by querying the terminal, and **fails
closed** when it cannot (headless, or a non-kitty run): Dispatch and `send` refuse
unless `--force` is passed (`README.md`; ADR 0011). That is a correct safety
default, but it makes agent-driven and CI-driven dispatch fragile — an agent
dispatching from another terminal must either see the same tabs or guess `--force`.

## Design question (decide and record in `## Comments` before coding)

Whether to adopt a headless execution mode per runtime, and which mechanism:

- (a) **Claude Agent SDK / headless `-p`** and **Codex non-interactive mode** —
  run the agent without a terminal at all.
- (b) **opencode `run` against the daemon** (already partly done via ticket 04) —
  no PTY needed.
- (c) **Leave it** — accept `--force` as the contract for headless runs and mark
  this `wontfix`.

For a single-user local tool, (c) is a legitimate answer. File the decision either
way.

## Solution (if (a) or (b))

Add a headless execution mode so a Run's liveness and control come from the
runtime's API, not from terminal detection, and the `--force` path becomes a
non-issue rather than a guess.

## Commits

1. Record the decision in `## Comments` (this may be the whole ticket if (c)).
2. If proceeding: add the headless execution mode for the chosen runtime(s),
   reusing the process-group and session-liveness rules already in place.
3. Update the occupancy rule so a headless Run is judged by its session/process,
   not by terminal detection.
4. Tests; docs; changelog.

## Acceptance Criteria

- [x] The decision is recorded, with reasoning, in `## Comments`.
- [x] If implemented: a dispatch with no terminal available succeeds without
      `--force`, and an open Workspace is still protected. — Not applicable:
      nothing was implemented. The mode this describes already exists (see the
      decision), so there is nothing to add.
- [x] Existing terminal-driven behaviour is unchanged.
- [x] `mise run check` is green.

## Out of Scope

- The terminal occupancy check itself (ADR 0011) — this only removes the need to
  guess when headless.

## References

- `README.md` (the `--force` rule), ADR 0011, ADR 0009
- Claude headless: https://code.claude.com/docs/en/headless
- Codex action: https://learn.chatgpt.com/docs/github-action
- opencode GitHub action: https://opencode.ai/docs/github/

## Comments

- 2026-10-15: Filed from the strategy review. May resolve as `wontfix`; filed
  because it is the cleanest fix for the "fail closed" ergonomics of the
  dispatch loop.
- 2026-10-15: **Decision: (c) `wontfix`.** The headless execution mode this
  ticket proposes already exists for every Agent Runtime, so (a) and (b) are
  already the implementation. The only terminal dependency left in the Dispatch
  path is the ADR-0011 occupancy query, which this ticket explicitly puts out of
  scope - so there is no mode to add, only a safety default to weaken.

  Verified before deciding:

  - **Every runtime already runs non-interactively, with no PTY.** Claude is
    `claude -p --output-format stream-json --verbose` (`claude_command`), Codex
    is `codex exec` (`codex_command`), opencode is `opencode run --format json
    --auto` (`opencode_command`), and Pi is `pi --mode json` (`pi_command`), all
    in `src/engine/runtime.rs`. Option (a) *is* the implementation.
  - **Runs are already judged by process, not terminal.** The supervisor spawns
    each Run with `process_group(0)` and tracks liveness through the process
    group, the structured log, and the persisted session. A foreground Run
    already degrades cleanly with no terminal: `Handover::capture` returns
    `None` when stdin is not a terminal (`src/engine/foreground.rs`).
  - **opencode is already API-native for control.** `send`/follow-up and
    permission replies go through `opencode api` against the daemon (ticket 04),
    not a PTY. Option (b) *is* the implementation.
  - So `--force` is the only terminal-dependent thing in Dispatch, and it fails
    closed exactly as ADR 0011 decided.

  Why not remove the fail-closed default anyway:

  - **It is not a guess when it fires.** jjfx refuses only when it cannot see a
    terminal at all, and with no terminal there are no `jjfx:<worker>` tabs to
    collide with - so `--force` states the truth rather than guessing it.
  - **It is rare, and it names its own remedy.** Runs inherit the environment, so
    an agent inside a Worker Workspace keeps the `KITTY_LISTEN_ON` jjfx was
    launched with; a configured `terminal.listen_on` resolves a live socket from
    anywhere on the machine. When it does fire, the error says to re-run with
    `--force`.
  - **Weakening it trades a real safety property for ergonomics.** Without the
    terminal, the only fallback signal is the durable occupancy marker, which
    knows only the Workspaces *jjfx* opened - a Workspace a human opened by hand
    would become dispatchable. That is precisely the collision ADR 0011 exists to
    prevent, and revisiting an accepted ADR is a larger change than this ticket
    asks for.

  What would change the answer: jjfx gaining multi-user or CI dispatch as a real
  use case, or the fail-closed path firing while a human genuinely has a
  Workspace open. The design to reach for then is that marker fallback - exclude
  what jjfx opened, keep failing closed only when the marker is unreadable - a
  scoped change to `dispatch_exclusions` (`src/cli.rs`) and `reconcile_occupancy`
  (`src/engine/eligibility.rs`), which deserves its own ticket rather than a
  speculative change here.

  No code, docs, or changelog change: nothing user-facing changed, so there is
  nothing for `momo` to record.
