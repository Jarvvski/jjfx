# `jjfx dispatch` has no `--json`

Status: resolved
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

- [x] `jjfx dispatch --json` prints a machine-readable result for every Ticket in the request, including the ones not dispatched.
- [x] Each entry names the Ticket, its outcome, and the Worker when one took it.
- [x] Without `--json` the prose output is unchanged.
- [x] Tests cover a dispatched Ticket, a held-back Ticket, and a failure.
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
- 2026-10-16: Implemented `dispatch --json`. `DispatchEntry` in `src/cli.rs` is the shared outcome model both the prose and JSON renderings read, built once from `DirectDispatchOutcome`, so a new engine outcome breaks compilation in `from_outcome` rather than reaching only one rendering. Each object carries `ticket`, `outcome` (`dispatched`/`failed`/`waiting`/`orchestrating`), `worker` (null when none took it), `phase`, `pid` and `detail`. The single-Ticket orchestration handoff also reports JSON, and `--json` with no discovered Tickets prints `[]`. Unit tests cover a dispatched, a failed and a held-back Ticket plus the unchanged prose; an integration test asserts the empty batch on stdout.

## Resolution

Reconciled Mon 19 Oct 2026 against the merged code at `9d6bd0ce6e7b`.

This ticket was marked resolved without its acceptance boxes ticked. Every criterion was re-verified by an independent read-only review of the code and tests on `main`. The reconciliation recorded no new work; it confirms what shipped.

Evidence: `DispatchArgs` carries `json: bool` (`src/cli.rs:192`), and `render_dispatch_entries` (`:3861`) routes to `dispatch_json` (`:3941`) or the unchanged `dispatch_prose` (`:3870`) from the one `DispatchEntry` model. Criterion 4 holds: `dispatch_json_names_each_ticket_outcome_worker_and_detail` (`:6221`), `dispatch_json_reports_a_skipped_ticket_with_its_reason` (`:6249`), `dispatch_json_is_an_empty_array_when_no_ticket_was_requested` (`:6244`) and `dispatch_parser_accepts_json` (`:5215`), plus the integration coverage in `tests/dispatch_outcomes.rs`. Criterion 3 holds because the prose path is a separate rendering of the same entries, unchanged.

Not ticked: the `mise run check` criterion. It is a landing-commit property, and this reconciliation deliberately does not run the full suite (the orchestrator runs the gate once at the end).
