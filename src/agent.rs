//! The agent lifecycle axis (ADR 0002/0003), event-sourced from Claude Code
//! hooks. Hooks append raw events to a global JSONL log (ADR 0004); this module
//! parses each line and folds it into a per-workspace [`AgentState`], keyed by
//! the event's `cwd` - the clean join to a workspace confirmed by spike 01.
//!
//! Only the three common fields (`hook_event_name`, `cwd`, `session_id`) are
//! read; no event-specific field is touched, so the un-captured field shapes of
//! `PermissionRequest`/`Notification` (spike 01's open item) never matter here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// What the agent in a workspace is doing right now (CONTEXT: agent lifecycle).
/// `Absent` is the default - a workspace jjfx has seen no live session for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentState {
    /// No live session (never started, or the log has no events for this cwd).
    #[default]
    Absent,
    /// A turn is in progress (between `UserPromptSubmit` and its `Stop`).
    Working,
    /// A turn finished; the session is present and awaiting the human.
    Waiting,
    /// Blocked on a permission or decision dialog.
    NeedsAttention,
    /// The session closed.
    Ended,
}

impl AgentState {
    /// Short, stable label for a list row.
    pub fn label(self) -> &'static str {
        match self {
            AgentState::Absent => "-",
            AgentState::Working => "working",
            AgentState::Waiting => "waiting",
            AgentState::NeedsAttention => "needs-attn",
            AgentState::Ended => "ended",
        }
    }
}

/// One hook event, reduced to the fields the fold needs: the event name and the
/// `cwd` that joins it to a workspace. Extra JSON fields (including `session_id`,
/// unneeded while a workspace hosts at most one agent) are ignored, so the same
/// struct parses every event type.
#[derive(Debug, Clone, Deserialize)]
pub struct Event {
    #[serde(rename = "hook_event_name")]
    pub name: String,
    pub cwd: String,
}

/// Parse one JSONL line into an [`Event`], or `None` for a blank/malformed line
/// (the tail must survive a partial or garbage line without crashing the TUI).
pub fn parse_line(line: &str) -> Option<Event> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    serde_json::from_str(line).ok()
}

/// The event -> agent-state transition map confirmed in spike 01. Unknown events
/// (the wider 2.x set jjfx does not model) leave the state unchanged.
pub fn transition(current: AgentState, event: &str) -> AgentState {
    match event {
        "SessionStart" => AgentState::Waiting,
        "UserPromptSubmit" => AgentState::Working,
        "Stop" | "StopFailure" => AgentState::Waiting,
        "PermissionRequest" => AgentState::NeedsAttention,
        "SessionEnd" => AgentState::Ended,
        _ => current,
    }
}

/// Canonicalize a path for use as a join key, falling back to the path as-is
/// when it cannot be resolved (e.g. a workspace dir that no longer exists). Both
/// event `cwd`s and workspace paths pass through this so they compare equal.
pub fn canon(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Replay a sequence of events into the current per-workspace agent state, keyed
/// by canonicalized `cwd`. At most one agent runs per workspace (CONTEXT), so
/// last-write-wins by log order is the whole rule.
pub fn fold(events: impl IntoIterator<Item = Event>) -> HashMap<PathBuf, AgentState> {
    let mut map = HashMap::new();
    for ev in events {
        let key = canon(Path::new(&ev.cwd));
        let entry = map.entry(key).or_insert(AgentState::Absent);
        *entry = transition(*entry, &ev.name);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_fields_and_ignores_the_rest() {
        let line = r#"{"session_id":"s1","transcript_path":"/t","cwd":"/w/a","hook_event_name":"UserPromptSubmit","prompt":"hi"}"#;
        let ev = parse_line(line).unwrap();
        assert_eq!(ev.name, "UserPromptSubmit");
        assert_eq!(ev.cwd, "/w/a");
    }

    #[test]
    fn blank_and_malformed_lines_parse_to_none() {
        assert!(parse_line("").is_none());
        assert!(parse_line("   ").is_none());
        assert!(parse_line("not json").is_none());
        assert!(parse_line("{}").is_none()); // missing required cwd/name
    }

    #[test]
    fn transitions_follow_the_spike_map() {
        use AgentState::*;
        assert_eq!(transition(Absent, "SessionStart"), Waiting);
        assert_eq!(transition(Waiting, "UserPromptSubmit"), Working);
        assert_eq!(transition(Working, "Stop"), Waiting);
        assert_eq!(transition(Working, "StopFailure"), Waiting);
        assert_eq!(transition(Waiting, "PermissionRequest"), NeedsAttention);
        assert_eq!(transition(Working, "SessionEnd"), Ended);
        // An event jjfx does not model leaves the state untouched.
        assert_eq!(transition(Working, "PreToolUse"), Working);
    }

    #[test]
    fn fold_replays_a_full_turn_per_cwd() {
        let lines = [
            r#"{"cwd":"/w/a","hook_event_name":"SessionStart"}"#,
            r#"{"cwd":"/w/a","hook_event_name":"UserPromptSubmit"}"#,
            r#"{"cwd":"/w/b","hook_event_name":"SessionStart"}"#,
            r#"{"cwd":"/w/a","hook_event_name":"Stop"}"#,
        ];
        let events = lines.iter().filter_map(|l| parse_line(l));
        let state = fold(events);
        // /w/a: Start -> Working -> Waiting; canon() no-ops on nonexistent paths.
        assert_eq!(state.get(Path::new("/w/a")), Some(&AgentState::Waiting));
        assert_eq!(state.get(Path::new("/w/b")), Some(&AgentState::Waiting));
    }
}
