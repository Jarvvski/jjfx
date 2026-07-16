//! The agent lifecycle axis (ADR 0002/0003), event-sourced from agent hooks.
//! Claude Code and Codex emit the same event names and payload fields, so one
//! fold serves both (Codex just lacks `SessionEnd`, see `hooks.rs`). Hooks
//! append raw events to a global JSONL log (ADR 0004); this module parses each
//! line and folds it into a per-workspace [`AgentState`], keyed by the event's
//! `cwd` - the clean join to a workspace confirmed by spike 01.
//!
//! Only the common fields (`hook_event_name`, `cwd`, and `transcript_path` -
//! whose location under `~/.claude/` vs `~/.codex/` names the agent) are read;
//! no event-specific field is touched, so the un-captured field shapes of
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

/// Which CLI a session's events come from, derived per payload from
/// `transcript_path`: Claude Code transcripts live under `~/.claude/`, codex
/// rollouts under `~/.codex/`. `Unknown` covers lines without the field (both
/// agents send it on every event, so this is a malformed-line fallback).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentKind {
    #[default]
    Unknown,
    Claude,
    Codex,
}

impl AgentKind {
    /// The agent's name for a list row, with a neutral fallback.
    pub fn label(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Unknown => "agent",
        }
    }

    /// Derive the kind from a payload's `transcript_path`.
    fn from_transcript_path(path: &str) -> Self {
        if path.contains("/.claude/") {
            AgentKind::Claude
        } else if path.contains("/.codex/") {
            AgentKind::Codex
        } else {
            AgentKind::Unknown
        }
    }
}

/// One workspace's live agent: what it is doing, and which CLI it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Agent {
    pub state: AgentState,
    pub kind: AgentKind,
}

/// One hook event, reduced to the fields the fold needs: the event name, the
/// `cwd` that joins it to a workspace, and the `transcript_path` whose location
/// discriminates claude from codex. Extra JSON fields (including `session_id`,
/// unneeded while a workspace hosts at most one agent) are ignored, so the same
/// struct parses every event type.
#[derive(Debug, Clone, Deserialize)]
pub struct Event {
    #[serde(rename = "hook_event_name")]
    pub name: String,
    pub cwd: String,
    #[serde(default)]
    pub transcript_path: Option<String>,
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
fn transition(current: AgentState, event: &str) -> AgentState {
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
fn canon(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The per-workspace agent state, folded from the hook-event log and keyed by
/// canonicalized `cwd`. Owns the map, the per-event fold step, and the canon
/// join, so startup replay ([`replay`](Self::replay)) and live updates
/// ([`apply`](Self::apply)) reduce through the same rule and canonicalization
/// happens in exactly one place. At most one agent runs per workspace (CONTEXT),
/// so last-write-wins by log order is the whole rule.
#[derive(Debug, Default)]
pub struct AgentStates {
    states: HashMap<PathBuf, Agent>,
}

impl AgentStates {
    /// Startup: replay a sequence of events into a fresh map.
    pub fn replay(events: impl IntoIterator<Item = Event>) -> Self {
        let mut this = Self::default();
        for ev in events {
            this.apply(&ev);
        }
        this
    }

    /// Live: fold one event into the state, keyed by its canonicalized `cwd`.
    pub fn apply(&mut self, ev: &Event) {
        let key = canon(Path::new(&ev.cwd));
        let entry = self.states.entry(key).or_default();
        entry.state = transition(entry.state, &ev.name);
        // Every real payload carries the kind; keep the last known one so a
        // field-less line cannot wipe it (and a workspace that switches CLIs
        // updates on the new agent's first event).
        let kind = ev
            .transcript_path
            .as_deref()
            .map(AgentKind::from_transcript_path)
            .unwrap_or_default();
        if kind != AgentKind::Unknown {
            entry.kind = kind;
        }
    }

    /// The live agent for a workspace `path`, canonicalized to match the `cwd`
    /// keys so the two sides of the join compare equal. Default (`Absent`,
    /// `Unknown`) if the log has no events for it.
    pub fn agent_for(&self, path: &Path) -> Agent {
        self.states.get(&canon(path)).copied().unwrap_or_default()
    }
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
        assert_eq!(ev.transcript_path.as_deref(), Some("/t"));

        // A line without transcript_path still parses (the field is optional).
        let ev = parse_line(r#"{"cwd":"/w/a","hook_event_name":"Stop"}"#).unwrap();
        assert!(ev.transcript_path.is_none());
    }

    #[test]
    fn kind_derives_from_the_transcript_location() {
        assert_eq!(
            AgentKind::from_transcript_path("/Users/u/.claude/projects/x/s.jsonl"),
            AgentKind::Claude
        );
        assert_eq!(
            AgentKind::from_transcript_path("/Users/u/.codex/sessions/2026/07/16/rollout.jsonl"),
            AgentKind::Codex
        );
        assert_eq!(
            AgentKind::from_transcript_path("/somewhere/else.jsonl"),
            AgentKind::Unknown
        );
    }

    #[test]
    fn fold_keeps_the_last_known_kind() {
        let mut states = AgentStates::default();
        states.apply(&Event {
            name: "SessionStart".to_string(),
            cwd: "/w/a".to_string(),
            transcript_path: Some("/u/.claude/projects/x/s.jsonl".to_string()),
        });
        assert_eq!(states.agent_for(Path::new("/w/a")).kind, AgentKind::Claude);

        // A field-less line advances the state but cannot wipe the kind.
        states.apply(&Event {
            name: "UserPromptSubmit".to_string(),
            cwd: "/w/a".to_string(),
            transcript_path: None,
        });
        let agent = states.agent_for(Path::new("/w/a"));
        assert_eq!(agent.state, AgentState::Working);
        assert_eq!(agent.kind, AgentKind::Claude);

        // The workspace switches CLIs: the new agent's first event retags it.
        states.apply(&Event {
            name: "SessionStart".to_string(),
            cwd: "/w/a".to_string(),
            transcript_path: Some("/u/.codex/sessions/r.jsonl".to_string()),
        });
        assert_eq!(states.agent_for(Path::new("/w/a")).kind, AgentKind::Codex);
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
    fn replay_folds_a_full_turn_per_cwd() {
        let lines = [
            r#"{"cwd":"/w/a","hook_event_name":"SessionStart"}"#,
            r#"{"cwd":"/w/a","hook_event_name":"UserPromptSubmit"}"#,
            r#"{"cwd":"/w/b","hook_event_name":"SessionStart"}"#,
            r#"{"cwd":"/w/a","hook_event_name":"Stop"}"#,
        ];
        let events = lines.iter().filter_map(|l| parse_line(l));
        let states = AgentStates::replay(events);
        // /w/a: Start -> Working -> Waiting; canon() no-ops on nonexistent paths.
        assert_eq!(
            states.agent_for(Path::new("/w/a")).state,
            AgentState::Waiting
        );
        assert_eq!(
            states.agent_for(Path::new("/w/b")).state,
            AgentState::Waiting
        );
    }

    #[test]
    fn agent_for_an_unseen_path_is_absent() {
        let states = AgentStates::default();
        assert_eq!(
            states.agent_for(Path::new("/w/never")).state,
            AgentState::Absent
        );
    }

    #[test]
    fn apply_advances_a_live_event_through_the_same_fold() {
        let mut states = AgentStates::default();
        for name in ["SessionStart", "UserPromptSubmit"] {
            states.apply(&Event {
                name: name.to_string(),
                cwd: "/w/a".to_string(),
                transcript_path: None,
            });
        }
        assert_eq!(
            states.agent_for(Path::new("/w/a")).state,
            AgentState::Working
        );
    }
}
