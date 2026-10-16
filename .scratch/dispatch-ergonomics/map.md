# Dispatch ergonomics — the batch from the `todo-md` test-drive

## What this is

On 2026-10-16 a dedicated agent drove `jjfx` end to end against a throwaway repository (`/Users/jarvis/Code/personal/todo-md`): a five-ticket vanilla-JS app, local markdown tickets, no Linear. It implemented all five tickets through jjfx's Worker pool, and wrote up what it found (`/Users/jarvis/Code/personal/todo-md-env/REPORT.md`).

This effort is the findings, turned into work. Each ticket is self-contained and cites the real command output or source location that motivated it.

## The shape of the problem

Two themes, and they are the same theme:

1. **jjfx assumes a lot about the repository it is dropped into.** A bookmark named `main`, an `origin` remote, a tracker that can resolve a Ticket. A fresh repo has none of these, and the failures do not name what is missing. The default Ticket source is `linear`, so "out of the box" currently means "not at all".
2. **Dispatch reports success when nothing happened.** A failed batch exits 0. A background orchestration that dies instantly prints `orchestrating`. An empty ready set prints `[]`. A finished Worker does not resolve its Ticket, so a dependency chain deadlocks with no diagnostic.

Both themes cost the test-drive real time, and both are invisible unless you read the source.

## Tickets

| # | Ticket | Area |
| --- | --- | --- |
| 01 | Take a Ticket from free text, a file path, or stdin | `cli.rs`, `ticket_source.rs` |
| 02 | Dispatch must work with no `main` bookmark and no remote | `engine/workspace.rs`, `direct_dispatch.rs` |
| 03 | Local Tickets must resolve without a later Dispatch | `cli.rs`, `engine/state.rs` |
| 04 | Dispatch must not report success when nothing happened | `cli.rs` |
| 05 | Orchestration must work for a local feature Parent | `cli.rs`, `engine/local_tickets.rs` |
| 06 | `--all` must base a dependent on its blocker | `engine/orchestration.rs`, `cli.rs` |
| 07 | The ready set must say why it is empty | `engine/local_tickets.rs`, `cli.rs` |
| 08 | Capture Run Records and spend | `engine/state.rs`, `engine/opencode_api.rs` |
| 09 | Small dispatch frictions: help, prompt and version | `cli.rs`, `cli_help.rs` |
| 10 | Stop the test suite leaking agent processes | `tests/` |

## Deliberately not filed

- **Addressing a historical Run.** After `jjfx pool reset`, a Worker's earlier branches are unreachable from `open-pr`/`rebase`/`absorb`. Real, but it is a design decision about Worker identity rather than a bug, so it wants an owner ruling before an agent guesses. Filed separately if wanted.
- **`[tickets]` cannot be repository-local.** Ticket 01 works around it; whether `.jjfx/config.toml` should grow a second section is an owner decision, because `deny_unknown_fields` there is deliberate.

## Note on parallelism

Almost every ticket touches `src/cli.rs`. The test-drive showed agents can rebase, but overlapping work in one file is the main cost of a wide batch — expect to serialise some of these, or to accept rebases.
