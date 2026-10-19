# A Ticket is done only when a check actually ran and passed

Status: resolved
Type: task
Blocked by: none

## Problem

`resolved` is written when a Run completes with a successful conclusion - the
agent's own terminal report. Nothing verifies that the work is correct, or even
finished. In a real drive, all four Tickets read `resolved` and none of the work
was on trunk.

This is the trust boundary the whole loop rests on, and the research is
unusually clear about it. Anthropic's own example: "a flight-booking agent might
say 'Your flight has been booked'... the outcome is whether a reservation exists
in the environment's SQL database." Huang et al. show LLMs cannot self-correct
without external feedback, and sometimes get worse. The Replit post-mortem is
the cautionary case: production and dev shared a database, so *no prompt could
have prevented it* - the fix was architectural.

Ticket 18 already built most of the mechanism: a repo-defined `[checks]` gate
that runs between weld and push. What is missing is the wiring - the Ticket
status still reflects the run, not the check.

## Acceptance Criteria

- [x] A Ticket reaches its done status only when the repository's check ran
      against the change and passed; a Run that finishes with no check having
      run does not read as done.
- [x] A failed check leaves the Ticket demonstrably not-done, naming what failed.
- [x] A repository with no check configured behaves predictably and says so
      rather than silently claiming verification.
- [x] `dogfood/06` is closed by this, pointing at the decision.

## Resolution

Wired the run's done status to the repository's check, reusing ticket 18's
mechanism rather than adding a second check path.

- **The check's outcome is recorded on the Run Record.** A Run finalizes from
  the agent runtime's own terminal event, and that is exactly where the change
  is now verified: `run_record` (`src/engine/pool.rs`) runs the repository's
  `[checks]` command (`src/checks.rs`) against the Run's workspace - the
  change - and stores a typed `RunCheck` on the record: `Passed`,
  `Failed { command, detail }`, `CouldNotRun { command, detail }`, or
  `NotConfigured`. A failed Run is already not-done, so no check is run for it.
- **A Ticket is done only on a passing check.**
  `RunRecordRepository::verified_tickets` (`src/engine/state.rs`) returns only
  Tickets whose most recent Run both succeeded **and** carries
  `RunCheck::Passed`; the local Ticket writeback (`src/ticket_source.rs`)
  resolves exactly those. A record with no check - an older record, or a Run
  that never had one run - is not verified either.
- **Not-done is loud.** A finished Run whose check failed, could not run, or was
  not configured leaves the Ticket as it was and the writeback returns the
  reason (`Ticket <id> is not verified: check: \`cargo check\` failed: ...`, or
  `no check is configured for this repository`). `jjfx status`, `jjfx pool list`
  and Dispatch print it; `jjfx ticket reconcile` exits non-zero, because a
  reconcile that could not mark a finished Run done did not do its job.
- **A repository with no check configured** never auto-resolves its local
  Tickets: nothing ran to verify them, so jjfx does not silently claim it did.
  It says so on every reconcile, and dispatch, claiming, dependency readiness
  and the rest of the loop are unaffected - the repository still works; it just
  never marks work done on the agent's word alone.
- **`[checks]` reaches the Run through `Repository`.** `Repository` gained a
  `checks` field (defaulting to none configured, so tests and any caller that
  never sets one verify nothing), set from `Config::checks_for` in the CLI and
  from `DispatchSettings.checks` in the TUI adapter.
- **`dogfood/06` is closed.** Its reference already pointed at this ticket; its
  three criteria now hold and are ticked, and its `## Resolution` names this
  work.

Decisions:

- **The check runs against the worker's change at finalization**, not only in
  the forge. Ticket 18 runs it post-weld in the forge for the cross-PR hazard;
  this ticket is the broader trust boundary, and the reconcile path - the one
  that actually writes a Ticket's status - is where the reference list points
  (`src/checks.rs`, `src/engine/pool.rs` reconcile,
  `src/engine/local_tickets.rs`). Both reuse the same `checks::run`, so there is
  one check implementation.
- **"No check configured" fails closed.** Criterion 1 is explicit that a Run
  with no check having run does not read as done, so an unconfigured repo's
  Tickets are not resolved; criterion 3 is met by saying so rather than claiming
  verification.

Verified with `mise run check` (changelog validation + fmt + lint + build +
test), green. New tests: `checks`'s `RunCheck` mapping; `engine::state`'s
`verified_tickets`/`latest_ticket_records`; `engine::pool`'s
`a_finished_run_records_the_check_that_ran_against_its_change`,
`a_run_with_no_check_configured_records_that_nothing_verified_it`,
`a_failed_run_records_no_check`; `ticket_source`'s failed-check, no-check and
no-record cases; and the integration test
`ticket_reconcile_refuses_to_resolve_a_run_whose_check_did_not_pass`.

## References

- `.scratch/dogfood/issues/06-resolved-means-run-finished-not-landed.md`
- `.scratch/opinionated-loop/issues/18-forge-build-check.md`
- `src/checks.rs`, `src/engine/pool.rs` (reconcile), `src/engine/local_tickets.rs`
