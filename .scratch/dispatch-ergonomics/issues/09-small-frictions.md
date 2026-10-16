# Small dispatch frictions: help, prompt and version

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

Three small things, all from a test-drive, all cheap.

**1. The recovery hint points at a command `help` disowns.**

```
$ jjfx status --json
Error: No pool. Run: jjfx pool create --size N
$ jjfx help pool create
Error: unknown command: pool create. Run 'jjfx help' for the command list.
```

`pool create` is accepted by the parser (`src/cli.rs:473`: `"create" | "c" | "resize" | "r"`), but there is no help page for it - `src/cli_help.rs` lists `pool`, `pool list`, `pool rm`, `pool reset`, `pool profile`, `pool destroy`. The drift test added alongside per-command help only asserts that commands named in the *top-level* help have a page, and the aliases are never named there, so they slip through. This is the very first error a new user sees.

**2. The capacity prompt is a landmine for scripts.**

```
Pool has 0 idle worker(s) but 1 ticket(s) to dispatch. Grow pool by 1? [Y/n]
```

`confirm_growth` (`src/cli.rs:1748-1755`) returns true unless the input is exactly `n` or `no`, so a non-interactive caller with no stdin - or an empty answer - *grows the pool*. A prompt that cannot be answered must not default to the expensive option.

**3. The version is misleading.**

```
$ jjfx version
0.55.0
```

for a binary built from `main` after the 0.55.0 release, containing features 0.55.0 does not have. Bug reports become ambiguous about which build is in hand.

## Solution

- Give `pool create` and `pool resize` help pages, or make the recovery hint name a documented command.
- Extend the drift test to cover the parser's accepted commands, not only the top-level help's.
- Make the capacity prompt safe for a non-interactive caller: default to *no* when stdin is not a terminal or the answer is empty, or require an explicit `--grow`/`--no-grow`.
- Make `jjfx version` distinguish a development build from a released one (a git suffix, a `-dev` marker, whatever fits the existing versioning).

## Acceptance Criteria

- [ ] `jjfx help pool create` and `jjfx help pool resize` print a page - or the recovery hint names a documented command.
- [ ] The help drift test covers every command the parser accepts.
- [ ] A non-interactive caller cannot grow the pool by not answering the prompt.
- [ ] `jjfx version` distinguishes a development build from a release.
- [ ] `mise run check` is green.

## References

- `src/cli.rs:473` (the pool aliases), `:1180` (the recovery hint), `:1748` (`confirm_growth`)
- `src/cli_help.rs` (`TOP_LEVEL`, `PAGES`)
- `Cargo.toml`, `momo` versioning

## Comments

- 2026-10-16: Filed from the `todo-md` test-drive.
