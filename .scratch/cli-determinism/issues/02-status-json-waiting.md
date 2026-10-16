# `jjfx status --json` omits the held-back Tickets

Status: resolved
Type: task
Blocked by: none

## Problem

`jjfx status` prints a `Waiting:` line naming the Tickets a capped Dispatch held back, but the `--json` output does not carry them. The JSON is the machine-readable surface an agent is told to read terminal state from, so a ticket that was deliberately not dispatched is invisible in it - the dispatch simply looks like it did not happen.

The same asymmetry applies to any future presentation state: the human table can grow a line and the JSON quietly stays behind it.

## Evidence

- `src/cli.rs`: `if !snapshot.waiting().is_empty() { println!("Waiting: {}", snapshot.waiting().join(", ")); }` - human output only.
- `pool_list_json` emits one object per Worker with `worker`, `name`, `status`, `stuck`, `ticket`, `elapsed`, `error`, `exit_code`, `log_file`, `cost_usd` - and nothing about the waiting set.
- The waiting set is real persisted state, not a rendering detail: `src/engine/pool.rs` carries it on the snapshot (`SnapshotState.waiting`, `snapshot.waiting()`), written from `.jj/pool/waiting.json`.

## Solution

Carry the waiting set in the JSON. The current output is a bare array, which has no room to grow, so the shape has to change:

1. **Wrap it** - emit `{"workers": [ ... ], "waiting": ["AMBA-42", ...]}`. Extensible, and the standard shape for this; costs a breaking change to the JSON output (`momo break`), which is cheap pre-1.0 but must be named in the changelog.
2. **Keep the array and add a sibling command** - e.g. `jjfx status --waiting --json`. No break, but two calls and two shapes for one question.

Recommend 1. Whatever is chosen, the JSON should be able to carry every line the human table prints, so the next presentation addition does not repeat this gap.

## Acceptance Criteria

- [x] The held-back Tickets are readable from `jjfx status --json` (or the documented replacement for it).
- [x] The chosen shape is documented in `README.md` / the help, and the changelog records a break if the shape changed.
- [x] A test asserts the JSON carries the waiting set when a Dispatch held tickets back, and is empty/absent when it did not.
- [x] The human table output is unchanged.
- [x] `mise run check` is green.

## Out of Scope

- Changing when a Dispatch holds a ticket back (the `[spend] cap_usd` and `[dispatch] max_concurrent` gates) - this ticket is only about reporting it.

## References

- `src/cli.rs` (`pool_list_command`, `pool_list_json`)
- `src/engine/pool.rs` (`waiting`, `SnapshotState`)
- `src/render/pool.rs` (the TUI's own `waiting:` line)
- `.scratch/opinionated-loop/issues/14-stuck-watchdog-capacity-governor.md` (the cap that holds tickets back)

## Comments

- 2026-10-16: Filed from the `jjfx-dispatch` skill pass: the skill now points agents at `jjfx status --json` for state, and the waiting set is the one piece of state that is not there.
- 2026-10-16: Chose **option 1 (wrap)** - `jjfx status --json` now emits `{"workers": [...], "waiting": [...]}`. The bare array had nowhere to put the waiting set, which is pool-level rather than per-Worker, and the wrapper is the seam for the next line the human table grows (pool counts, spend summary) without another breaking change. The break is cheap pre-1.0 and recorded with `momo break`; the `jjfx-dispatch` skill and `README.md` were updated to read `.workers[]`. `waiting` is `[]` when no Dispatch held a ticket back. Human table output is unchanged.
