# Dogfood batch - jjfx driven against a fictional site

## What this is

A dogfood drive: jjfx was used to deliver a four-Ticket backlog in a throwaway
project (`dogfood-site`, a static site), following
`skills/jjfx-dispatch/SKILL.md`. Every finding below carries the command that
produced it and the output it produced; the full report is at
`/Users/jarvis/Code/personal/dogfood-site/dogfood-report.md`.

The **dispatch** half worked, and that is why this batch is about the landing
half. Three of the landing verbs failed, one of them silently and destructively.

## Tickets

| # | Ticket | Kind |
|---|---|---|
| 01 | `absorb` rewrote trunk | bug |
| 02 | the bookmark is resolved from the wrong source; a push of a missing bookmark succeeds | bug |
| 03 | `open-pr` guesses a forge slug from a filesystem path | bug |
| 04 | `pool profile` fails before the pool exists | bug |
| 05 | no read-only way to see Ticket readiness | feature |
| 06 | `resolved` means the run finished, not that the work landed | design |
| 07 | the skill's preconditions and ordering are wrong | docs |
| 08 | `colocation status` hung once, unreproduced | bug |

## What worked, and should not be refactored away

- `dispatch --all` naming the blocked Ticket on stderr, and keeping it out of
  stdout so `--json` still parses.
- The dependency materialised as a real stacked workspace, not a flag.
- Reconciliation reading the durable run record rather than live worker state.
- `wait` and `status --json` as clean machine-readable primitives.
- `help <command>` derived from the parser.

## Provenance

Filed from the drive's report, 2026-10-19. The severity-1 findings (01, 02, 03)
were verified against the merged source before filing - including re-running
`jj log -r 'trunk()'` and `jj log -r 'mutable() & bookmarks(exact:main)'` to
establish the precondition - rather than taken from the report on trust.
