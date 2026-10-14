# Bound and de-duplicate the terminal query

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

An external report (2026-10-14, `/tmp/jjfx-bug-kitty-ls-storm.md`) found a long-running `jjfx tui` spawning `kitten @ --to <socket> ls` in an unbounded, un-throttled loop: **71 concurrent `kitten @ ls` processes**, all children of the jjfx TUI, each ~2s old and continuously respawning, driving the target kitty to ~90-105% CPU. The user's symptom was "the visor is stuck". Killing the jjfx process and reaping the children restored it immediately (105.6% -> 5.7%, 71 -> 0 strays).

## Verified in source

The report's code claims all hold:

- `KittyTerminal::run` (`src/terminal.rs:240`) shells out via `cmd("kitten").args(...).run()`, and `Cmd::run` (`src/cmd.rs:96`) is `self.inner.output()` - **no timeout**. A slow kitty blocks the calling thread without bound.
- `open_workspaces` (`src/terminal.rs:342`) is `Ok(tab_names(&self.ls()?))` - **no caching, no single-flight**; every call is a fresh subprocess.
- It is reached from several paths that run during normal operation: `live_exclusions` (`src/workspace_dispatch/adapter.rs:125`, called at `:195` and `:211` on dispatch paths), `RealWorkspaceDispatch::refresh` (`:367-373`), `is_open` (`src/terminal.rs:336`), `agent_window` (`:326`), and startup/CLI (`src/lib.rs:184`, `src/cli.rs:1345,1367`).
- `spawn_worker_pool_poller` (`src/lib.rs:423`) submits a `Refresh` every 15s and on every nudge.

That is a positive-feedback loop: each call is blocking, so a slow kitty holds threads; concurrent callers each spawn their own `kitten`; the pile-up makes kitty slower still. The observed ~35 spawns/sec is higher than the 15s poller alone explains, so the *tightest* loop is not yet pinned - but the missing bound and the missing de-duplication are enough to explain the pile-up, and bounding the rate makes the exact source less critical.

## Impact

High. The host terminal (the visor) becomes unusable and a CPU core spins for the lifetime of the TUI (observed over ~5 days uptime). The only recovery is to kill jjfx and reap the children - or restart the terminal, which destroys every in-terminal session.

## Options

1. **Bound the terminal query.** Give the kitty query a timeout, so a stuck or slow kitty yields an `Err` rather than an unbounded block. Highest value on its own. Note this must be scoped to the terminal query, not applied to `Cmd::run` generally (agent Runs and `jj` operations are legitimately long).
2. **Single-flight + short TTL cache** for `kitten @ ls` in `KittyTerminal`, so concurrent callers share one in-flight query and reuse a recent result instead of each spawning a subprocess. This is what directly kills the stacking; with it, a separate rate limiter is arguably unnecessary.
3. **Backoff / rate-limit** the refresh and dispatch-exclusion paths when a query fails or is slow, so a bad query cannot become a hot re-spawn loop.
4. **Decouple** the query from the critical path (a dedicated thread with a snapshot the rest of the app reads), so a slow terminal never blocks dispatch or rendering. Larger; 1 and 2 make it less urgent.

## Acceptance Criteria

- [ ] A terminal query cannot block without bound: a slow or stuck kitty yields an error within a bounded time.
- [ ] Concurrent callers share one in-flight query and a recent result, so a slow kitty cannot stack subprocesses.
- [ ] A regression test with a stub `kitten` that sleeps asserts at most one concurrent `ls` and a bounded call rate over a simulated refresh storm. (`tests/worker_actions.rs` already stubs `kitten` and sets `KITTY_LISTEN_ON` - the harness to extend.)
- [ ] When the terminal is slow or unreachable, dispatch and the UI degrade gracefully - no unbounded block, no hot loop.

## Out of Scope

- `resolve_socket` (`src/terminal.rs:454`) is correct; the storm is not a socket-resolution bug.
- Whether `opencode serve` can also drive jjfx's kitty queries - unconfirmed in the report, and not needed to fix this.

## Comments

**Decided 2026-10-14: options 1 and 2 - bound the terminal query and single-flight it.**

- **Bound the query.** Give the kitty query a timeout so a stuck or slow kitty yields an `Err` rather than an unbounded block. Scoped to the terminal query, **not** to `Cmd::run` generally - agent Runs and `jj` operations are legitimately long.
- **Single-flight + short TTL cache** in `KittyTerminal` for `kitten @ ls`, so concurrent callers (the poller, dispatch exclusions, `refresh`, the UI) share one in-flight query and reuse a recent result instead of each spawning a subprocess.
- **Rejected for now:** a separate backoff/rate limiter (option 3) - de-duplication bounds the rate, so it would be redundant; and decoupling the query onto a snapshot thread (option 4) - larger, and 1 and 2 make it less urgent. Either can be revisited if the storm survives the fix.
- The regression test in the acceptance criteria is **required, not optional**: it is this ticket's reproduction, and it must prove the pre-fix storm red.
