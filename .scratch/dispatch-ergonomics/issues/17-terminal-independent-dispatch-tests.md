# Dispatch tests must not depend on the ambient terminal

Type: task
Status: ready-for-agent
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

- [ ] Dispatch tests pass with no reachable terminal (verified by running the suite with the terminal unavailable).
- [ ] No dispatch test depends on kitty being present.
- [ ] The chosen mechanism is documented where the next test author will see it.
- [ ] `mise run check` is green.

## Answer

<!-- recorded on resolution -->
