# Make the lifecycle event log tolerant and lossless

Status: resolved
Type: task
Blocked by: none

## Problem

- **T3** `read_events` did `fs::read_to_string(path).unwrap_or_default()`, so one
  invalid UTF-8 byte anywhere made the whole read fail and every agent showed
  `Absent`. A rotation slice cut at an arbitrary byte offset can produce exactly
  that. The live tailer already tolerated bad lines per-line.
- **T4** `run_tui` replayed the whole log and then started `watch_log`, which
  seeded its offset to the *current* file length. Hook lines appended between the
  two calls were neither replayed nor tailed.
- **T5** `rotate_if_needed` ran only at startup, so a long-lived TUI grew the log
  without bound. A second tailer saw a rotation's shrink and re-emitted the
  retained tail as duplicates.

## Fix

- `replay` reads the log as bytes and parses it line by line with
  `String::from_utf8_lossy`, so one torn line costs that line only. It returns
  the byte offset just past the last complete line.
- `watch_log` takes that offset instead of re-stat'ing the file, closing the
  startup window.
- The tailer rotates after each read and reseeds its offset past the retained
  tail it just emitted, so a long-lived TUI stays bounded without duplicates.
- A shrink (`len < offset`) now skips to the new end instead of resetting to 0:
  the retained tail is already-emitted history.

`read_events` was removed; its only callers were test modules, which now use
`replay(..).0`.

## Commits

1. `Make the event log tolerant, lossless, and bounded while running` - the
   lossy line reader, the replay offset, in-tailer rotation, the skip-on-shrink
   change, four tests, and a CHANGELOG `(fix)`.

## Testing Decisions

- `replay_tolerates_invalid_utf8_in_one_line` - a torn slice line between two
  good ones; the two good events survive. Pre-fix the whole read failed and the
  history was empty.
- `replay_offset_hands_appends_to_the_tailer` - a torn trailing line, then the
  completed append; the tailer seeded from the replay offset still sees it.
- `read_new_events_skips_a_rotated_shrink_without_re_emitting` - a shrink emits
  nothing and reseeds to the new end (previously it re-emitted the tail).
- `tailer_rotation_bounds_the_log_without_re_emitting` - 500 lines, one rotation,
  the file stays under the cap, nothing is re-emitted, and a later append is
  still seen.

## Known Residual Limitation

Rotation still reads the file and then renames a rewritten copy over it, so an
append landing in that microsecond window can be lost, and a second instance
whose offset happens to fall *inside* the retained tail can still see a few
duplicates. Closing those needs inter-process coordination (a lock the hooks
would also have to take), which is out of scope here: the events are state
hints, and the failure is now rare and bounded rather than systematic.

## Acceptance Criteria

- [x] One invalid byte does not wipe agent state.
- [x] No event appended during startup is lost.
- [x] A long-lived TUI bounds `events.jsonl`.
- [x] A second instance does not re-emit events after a full rotation shrink.
- [x] `mise run check` is green.

## Out of Scope

- Changing the event schema.
- Inter-process locking around rotation (see Known Residual Limitation).
