# Make the lifecycle event log tolerant and lossless

Status: ready-for-agent
Type: task
Blocked by: none

## Problem

The `events.jsonl` reader is whole-file UTF-8 and the tailer races startup:

- **T3** - `read_events` does `fs::read_to_string(path).unwrap_or_default()`
  (`events.rs:66-69`). One invalid UTF-8 byte anywhere makes the whole read
  fail, so every agent shows `Absent`. The live tailer tolerates bad lines
  per-line (`:163`). A rotation slice cut at an arbitrary byte offset
  (`:82-92`) can split a multi-byte char and trigger exactly this.
- **T4** - `lib.rs:211` reads the whole log, then `:216` starts `watch_log`,
  which seeds its offset to the *current* file length (`events.rs:119`). Hook
  lines appended between the two calls are neither replayed nor tailed.
- **T5** - `rotate_if_needed` runs only at startup (`lib.rs:210`), so a
  long-lived TUI grows the log without bound; rotation itself reads then renames
  (`events.rs:94-99`), discarding an append that lands in between, and a second
  tailer sees the shrink and re-emits the retained tail as duplicates
  (`:146-148`).

## Fix

- Read the log line-oriented and lossy end-to-end; never require whole-file
  UTF-8.
- Have the replay return the byte length it consumed and seed the tailer with
  it (no re-stat).
- Rotate inside the tailer, not only at startup, and handle a concurrent
  shrink/duplicate without re-emitting.

## Commits

1. Line-oriented lossy reader; per-line tolerance in the replay.
2. Seed the tailer from the replay offset.
3. Rotate in the tailer; handle shrink without duplicates.
4. Tests: invalid UTF-8 mid-file; an append during startup; rotation during a
   live session.
5. CHANGELOG entry `(fix)`.

## Acceptance Criteria

- [ ] One invalid byte does not wipe agent state.
- [ ] No event appended during startup is lost.
- [ ] A long-lived TUI bounds `events.jsonl`.
- [ ] A second instance does not re-emit events after a rotation.
- [ ] `mise run check` is green.

## Out of Scope

- Changing the event schema.
