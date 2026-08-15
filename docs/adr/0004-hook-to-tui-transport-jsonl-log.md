# Hook-to-TUI transport: a single append-only JSONL event log

Hooks append raw events (event name + payload) as one JSON line each to a single
global log at `${XDG_STATE_HOME:-~/.local/state}/jjfx/events.jsonl`. The TUI
tails it for live updates and rebuilds current state by replaying the log on
startup. Chosen over a local socket / HTTP endpoint (drops events whenever the
TUI is not running - fatal, since agents run while the dashboard is closed) and
over per-session state files (no ordering or history, a stale-file liveness bug,
and concurrent-write races).

## Consequences

- **Hooks stay dumb.** They only append; all state-machine logic lives in the
  Rust binary, so the global `~/.claude/settings.json` hook config is written
  once and never revised when the lifecycle logic changes.
- **One fixed global path** (not per-repo `.jj/`) keeps each hook a
  dependency-free `echo >>` with no repo-root resolution; the TUI filters events
  by `cwd`.
- Single-line JSON writes are below `PIPE_BUF`, so `O_APPEND` is atomic across
  concurrent agents without locking. Growth is bounded by size-based rotation.

## Pi transport amendment

The Pi lifecycle extension uses the same global JSONL path and appends one
compact, versioned jjfx envelope per event. It opens the path for each append so
log rotation cannot strand a long-lived Pi process on the renamed file. The
envelope normalizes Pi event names and includes explicit agent and session
identity; provider-specific extension payloads do not enter the Rust lifecycle
model. Installation writes only the jjfx-owned auto-discovered extension file.

Status: accepted (Pi amendment implemented by issue 05)
