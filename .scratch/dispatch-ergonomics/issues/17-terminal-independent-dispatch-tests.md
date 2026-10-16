# Dispatch tests must not depend on the ambient terminal

Type: task
Status: resolved
Blocked by: none

## Problem

`dispatch` fails closed when it cannot determine which Workspaces are open (ADR 0011): with no reachable terminal it refuses, naming `kitten @ ls` and suggesting `--force`. That is correct product behaviour — and it makes every test that dispatches **environment-dependent**, because the terminal seam runs for real.

`a_background_orchestration_for_a_local_feature_parent_names_the_child_log`, added by ticket 05, missed the `--force` that every other dispatch test in its file passes. It therefore passed on a developer machine running kitty and failed on CI, where `kitten @ ls` cannot be reached — and because it merged on a green-looking status, it left `main` red until a follow-up fixed it.

The convention ("remember `--force`") is the thing that failed. Nothing catches a new dispatch test that forgets it, and the failure is silent in the worst possible direction: green locally, red on CI.

## Solution

Make the terminal seam deterministic in tests rather than relying on the ambient environment. Options, roughly in order of preference:

- **Stub the terminal in the fixture.** `DispatchFixture` already owns a temporary `bin` directory that goes on `PATH` ahead of the real one; a fake `kitten` there that answers `@ ls` with an empty workspace list would make every dispatch test terminal-independent, and would exercise the real seam rather than bypassing it.
- **An env seam** telling the terminal check to treat "no terminal" as "no occupied Workspaces" in tests. Smaller, but tests then stop exercising the real path.
- **A guard test** asserting no dispatch test invokes `jjfx dispatch` without `--force` or a stubbed terminal. Cheapest, and it catches the regression rather than preventing it.

Whichever is chosen, drop the now-unnecessary `--force` flags where a stub makes them redundant, and keep the comment explaining the terminal check.

## Acceptance Criteria

- [x] Dispatch tests pass with no reachable terminal (verified by running the suite with the terminal unavailable).
- [x] No dispatch test depends on kitty being present.
- [x] The chosen mechanism is documented where the next test author will see it.
- [x] `mise run check` is green.

## Answer

Chose the first option: stub the terminal in the fixture. `tests/support/mod.rs`
gains `stub_terminal(bin)`, which writes a fake `kitten` into `bin` that answers
`kitten @ ls` with an empty JSON array, and returns a `PATH` with `bin` ahead of
the inherited one. An empty array is a terminal with no tabs open, so the
Dispatch Eligibility check (ADR 0011) sees no occupied Workspaces and Dispatch
proceeds without `--force` - while still running the real seam rather than
bypassing it.

Every Dispatch fixture now installs the stub. `DispatchFixture` in
`tests/dispatch_outcomes.rs` and `BareRepository` in `tests/adhoc_ticket.rs` put
it on `PATH` for every command they build, and `tests/local_tickets.rs` routes
its Dispatch invocations through a new `run_jjfx` helper that does the same. The
now-redundant `--force` flags are gone from the thirteen Dispatch invocations
that carried them (the other two never passed it); the comment explaining the
terminal check stays, next to the stub.

Evidence: with `kitten` removed from `PATH` and `KITTY_LISTEN_ON` unset, the
three files pass (4 + 5 + 10 tests). A negative control shows the stub is load
bearing - the same `jjfx dispatch --all --json` with no stub and no `--force`
fails closed with `cannot determine which Worker Workspaces are open`, then
proceeds past the terminal check once the stub is on `PATH`. `mise run check` is
green.

Documented on `stub_terminal` itself, in the shared toolbox a new Dispatch test
reaches for.
