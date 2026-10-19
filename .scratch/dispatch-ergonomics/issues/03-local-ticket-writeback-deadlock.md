# Local Tickets must resolve without a later Dispatch

Status: resolved
Type: task
Blocked by: none

## Problem

A finished Worker does not resolve its local Ticket, though the docs say it does:

> A Worker whose Run finishes sets its `Status:` to `resolved` and appends a dated line under the file's `## Comments` heading.

Observed: two Workers ran to completion (`status` → `done`, `exit_code: 0`) and their Tickets stayed `claimed`. Resolution is a *side effect of the next Dispatch*: `write_back_local_tickets` is called near the end of `dispatch_command` (`src/cli.rs:1603`, `:1627`), after the ready set is computed. When the ready set is empty the function returns early (`src/cli.rs:1576-1583`) and the writeback is never reached:

```rust
if ready.tickets().is_empty() {
    ...
    return Ok(());          // write_back_local_tickets is never reached
}
```

On a linear dependency chain that is a deadlock: the only way to get a next Dispatch is a ready Ticket, and the only way to get a ready Ticket is for the blockers to resolve.

```
$ jjfx dispatch --all --json
Fetching tickets with label 'ready-for-agent'...
[]
EXIT: 0
```

It is worse than a stall. `jjfx pool reset <worker>` - which you must run to reuse a `done` Worker - clears the Worker's ticket association, so resetting before the writeback runs destroys the `done` signal and the Ticket is stuck at `claimed` permanently. In the test-drive the only way to advance the chain was to run a dispatch that failed on purpose, and decline the capacity prompt, to reach the writeback without firing a run.

## Solution

Drive the writeback from a durable record rather than from a Dispatch that happens to have work.

- Run the writeback before the empty-ready early return, so a Dispatch with nothing to do still reconciles.
- Reconcile on the state-reading commands too: `jjfx status` / `pool list` already call `reconcile_runs` (`src/cli.rs:1177`); they should also write back.
- Add an explicit `jjfx ticket reconcile` (or `jjfx reconcile`) so the operation is a first-class command, not a side effect.
- Make the `done` signal survive `pool reset`: the Run Record (`.jj/pool/runs.jsonl`) is durable; live Worker state is not.

## Acceptance Criteria

- [x] A finished Worker's local Ticket is `resolved` without a second Dispatch being needed.
- [x] `jjfx dispatch --all` with an empty ready set still reconciles finished Tickets.
- [x] There is an explicit command to reconcile (or a documented equivalent) that needs no ready Ticket.
- [x] `pool reset` no longer destroys an unresolved Ticket's completion signal.
- [x] A test covers the chain: dispatch → run finishes → ticket resolved → dependent becomes ready.
- [x] `mise run check` is green.

## References

- `src/cli.rs:1576-1583` (the early return), `:1603`, `:1627`, `:2471` (the writeback call sites)
- `src/engine/local_tickets.rs` (`write_back`, `claim`, `resolve`)
- `src/engine/state.rs` (Run Records)

## Comments

- 2026-10-16: Filed from the `todo-md` test-drive. The worst finding in the run.
- 2026-10-16: Resolved. The completion signal is now the durable Run Record store: `RunRecordRepository::completed_tickets` returns the latest-succeeded Ticket per record, and the writeback resolves from that rather than live Worker state, so `pool reset` no longer destroys the signal. Dispatch reconciles before it discovers ready work (the empty-ready early return is now reached only after a reconcile), `jjfx status` / `jjfx pool list` reconcile on read, and `jjfx ticket reconcile` reconciles on demand with no Dispatch and no ready Ticket. Live `done` Workers are still folded in, so a Run whose record was never written (a runtime that finalizes lazily, the opencode case ticket 08 fixes) still resolves. An in-flight Ticket is never resolved from a stale record.

## Resolution

Reconciled Mon 19 Oct 2026 against the merged code at `6d6f3b689e0e`.

This ticket was marked resolved without its acceptance boxes ticked. Every criterion was re-verified by an independent read-only review of the code and tests on `main`, and `mise run check` was re-run green on `6d6f3b689e0e`. The reconciliation recorded no new work; it confirms what shipped.
