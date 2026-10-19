# Enforce dispatch capacity deterministically

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

Concurrency is bounded by an instruction, not by the tool. A root session told
"don't work on more than 3 bugs at a time" was measured at **6 concurrent**
children.

jjfx already has a pool size and a held-back set (`waiting.json`) for the last
dispatch, so the mechanism is half there - what is missing is a durable,
enforced limit that survives across dispatches and is visible.

## Acceptance Criteria

- [ ] A configured concurrency limit is enforced across dispatches, not just
      within one.
- [ ] A dispatch that would exceed it holds work back, and says what it held
      back and why.
- [ ] The limit is visible in `status --json` (how many are running, how many
      are held, what the limit is).
- [ ] A test drives more ready work than the limit and asserts the limit holds.

## References

- `src/engine/eligibility.rs`, `src/engine/pool.rs`
- `docs/adr/0012-named-worker-pools.md`
