# Drive runs without a terminal (remove the `--force` ambiguity)

Status: ready-for-agent
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

- [ ] The decision is recorded, with reasoning, in `## Comments`.
- [ ] If implemented: a dispatch with no terminal available succeeds without
      `--force`, and an open Workspace is still protected.
- [ ] Existing terminal-driven behaviour is unchanged.
- [ ] `mise run check` is green.

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
