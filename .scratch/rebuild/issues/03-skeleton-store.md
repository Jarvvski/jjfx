# Walking skeleton: workspace store, cache mirror, list render

Status: done

## Parent

epics/A-foundations.md

## What to build

The end-to-end skeleton the whole app hangs off: a ratatui + crossterm + tokio
application that loads the authoritative per-repo workspace store, reconciles it
with the existing `.jj/ws-cache` (reading it so shell-created workspaces appear,
writing through so the bash tools stay consistent - ADR 0006), watches the cache
file for live changes, renders a plain workspace list, and quits cleanly with a
restore-on-panic guard so a crash never corrupts the terminal. No lifecycle
state yet - just the workspaces and their paths.

## Acceptance criteria

- [x] Launching jjfx in a jj repo lists its workspaces (default + named), byte-compatible with `.jj/ws-cache` (`name\tpath`). (Verified: `--list` and PTY run show `default` + named; hexdump confirms `name\t...path\n` bytes, `awk -F'\t'` parses them.)
- [x] A workspace created in a shell (`jj ws add x`) appears in the running TUI without a restart. (PTY smoke test: `feature-live` absent before the cache write, present after, no restart. Native `jj workspace add` also surfaces via the jj-CLI reconcile - path shown empty since jj records no path; see note.)
- [x] The authoritative store round-trips: jjfx-side changes write through to `.jj/ws-cache` atomically (temp-file + rename), and the bash tools read them. (`cache::write_through` writes a same-dir temp then renames, skipping unchanged content; round-trip unit-tested; a tab-delimited reader parses it back.)
- [x] `q`/esc quits; a panic restores the terminal via the panic hook; `mise run check` passes. (PTY test: exit 0, alt-screen entered and left; panic hook chains `tui::restore` before the default hook. `mise run check` green, 12 tests.)

## Design note

jj exposes workspace *names* but not their filesystem *paths* (spike 02, re-confirmed here: `json(self)` and `jj workspace list` carry name + target only, and nothing in `.jj/` records a workspace's external path). So `name -> path` lives solely in `.jj/ws-cache`, exactly as ADR 0006 frames it. Existence is the union of the derived `default` (path = repo root), the ws-cache entries, and jj's known names; paths come only from the cache and the derived default. A separate on-disk store file (`.jj/jjfx/…`) is deferred until there is non-derivable state to hold (labels, pin order, forge history) - per ADR 0006, persist only what cannot be derived.

## Blocked by

None - can start immediately.
