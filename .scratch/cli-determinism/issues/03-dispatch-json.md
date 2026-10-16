# `jjfx dispatch` has no `--json`

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

`jjfx dispatch` reports what it did in prose: which Worker took which Ticket, which Tickets were held back, which failed. An agent that dispatched a batch has to parse sentences to learn the outcome, which is exactly the guesswork `jjfx status --json` removed for the pool.

Every other command that reports state has a machine-readable form - `jjfx pool list [--json]` and `jjfx status [--json]`. `dispatch` is the one that tells you what the dispatch actually did, and it is the one without it.

## Evidence

- `DispatchArgs` (`src/cli.rs`) has no `json` field; the parser accepts only the tickets, `--all`, `--fg`/`--bg`, `--provider`, `--model`, `--budget`, `--label`, `--no-orchestrate` and `--force`.
- The outcomes are printed as sentences: `src/cli.rs` prints `"  {} waiting: {}"` for a held ticket and `"  quota-blocked {ticket}: {} (waiting; not retried)"` / `"  waiting for capacity: {ticket}"` for the other two refusals.
- `pool list [--json]` / `status [--json]` exist and are documented in the top-level help; `dispatch` has no equivalent.

## Solution

Add `--json` to `dispatch`, emitting one object per Ticket it was given - including the ones it did not dispatch - with at least the ticket id, the outcome, the Worker when one took it, and the detail the prose path prints.

Build it from the same outcome model the prose is rendered from (`DirectDispatchOutcome` and the batch result), so a new outcome cannot be added to one path and forgotten in the other.

## Acceptance Criteria

- [ ] `jjfx dispatch --json` prints a machine-readable result for every Ticket in the request, including the ones not dispatched.
- [ ] Each entry names the Ticket, its outcome, and the Worker when one took it.
- [ ] Without `--json` the prose output is unchanged.
- [ ] Tests cover a dispatched Ticket, a held-back Ticket, and a failure.
- [ ] `mise run check` is green.

## Out of Scope

- `jjfx send` / `jjfx review` - they report one Worker's Run, not a batch outcome.
- Changing which Tickets a Dispatch takes.

## References

- `src/cli.rs` (`DispatchArgs`, `parse_dispatch`, `dispatch_command`, the outcome printing)
- `src/engine/worker_actions.rs` (`DirectDispatchOutcome`, `RunMode`)
- `README.md` §"Command surface"

## Comments

- 2026-10-16: Filed from the `jjfx-dispatch` skill pass, so an agent can read what a dispatch did instead of parsing its prose.
