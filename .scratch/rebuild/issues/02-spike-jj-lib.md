# Spike: match jj-lib to the installed jj and set the mise pin

Status: ready-for-agent

## Parent

epics/A-foundations.md

## What to build

Determine whether jjfx can link jj-lib against the jj version this machine runs
(ADR 0007). Identify the jj-lib release whose on-disk store format matches the
installed `jj`, prove it can open and read this repo's `.jj/` store in a
throwaway binary, and pin `jj` in `mise.toml` to that version so the two upgrade
in lockstep. If no clean match exists, exercise the documented fallback: read jj
state by shelling to `jj` with structured `-T` templates instead - which
reopens ADR 0007.

## Acceptance criteria

- [ ] Installed `jj` version and its store format identified; a candidate jj-lib release chosen.
- [ ] A throwaway binary links jj-lib and reads this repo's commit graph + working-copy state without error.
- [ ] `mise.toml` pins `jj` to the matching version; `mise run check` passes.
- [ ] If no clean match: a decision recorded to use CLI `-T` parsing for jj reads, ADR 0007 amended, and a minimal `-T` read proven instead.

## Blocked by

None - can start immediately.
