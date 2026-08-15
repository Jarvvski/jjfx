//! The agent lifecycle axis (ADR 0002/0003), event-sourced from agent hooks.
//! Claude Code and Codex append raw hook payloads while provider adapters can
//! append versioned jjfx envelopes. One fold serves every source through the
//! common event name and `cwd` join (ADR 0004). Legacy transcript locations
//! identify Claude and Codex; versioned envelopes carry explicit identity and
//! session fields without exposing provider-specific payloads to the fold.

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

/// Which CLI a session's events come from. Versioned jjfx envelopes carry an
/// explicit identity; legacy Claude Code and Codex payloads derive it from
/// their transcript locations. `Unknown` is the neutral malformed fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentKind {
    /// The event does not identify a supported agent.
    #[default]
    Unknown,
    /// Claude Code.
    Claude,
    /// OpenAI Codex.
    Codex,
    /// Pi coding agent.
    Pi,
}

impl AgentKind {
    /// The agent's name for a list row, with a neutral fallback.
    pub fn label(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Pi => "pi",
            AgentKind::Unknown => "agent",
        }
    }

    /// Parse an explicit jjfx lifecycle-envelope identity.
    fn from_name(name: &str) -> Self {
        match name {
            "claude" => AgentKind::Claude,
            "codex" => AgentKind::Codex,
            "pi" => AgentKind::Pi,
            _ => AgentKind::Unknown,
        }
    }

    /// Derive the kind from a legacy payload's `transcript_path`.
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
    /// The agent's current lifecycle state.
    pub state: AgentState,
    /// The agent implementation associated with the lifecycle state.
    pub kind: AgentKind,
}

/// One lifecycle event reduced to the provider-neutral fields the fold needs.
/// Legacy payloads use `transcript_path`; versioned jjfx envelopes use explicit
/// agent and session identity. Unrelated provider fields remain ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct Event {
    /// Provider-neutral lifecycle event name.
    #[serde(rename = "hook_event_name")]
    pub name: String,
    /// Workspace directory used to join the event to a jjfx row.
    pub cwd: String,
    /// Legacy Claude or Codex transcript location used for identity inference.
    #[serde(default)]
    pub transcript_path: Option<String>,
    /// Version of a jjfx-owned lifecycle envelope, if this is one.
    #[serde(default)]
    pub jjfx_event_version: Option<u64>,
    /// Explicit provider identity carried by a jjfx-owned envelope.
    #[serde(default)]
    pub agent_kind: Option<String>,
    /// Stable provider session identity carried by a jjfx-owned envelope.
    #[serde(default)]
    pub session_id: Option<String>,
}

/// Parse one JSONL line into an [`Event`], or `None` for a blank/malformed line
/// (the tail must survive a partial or garbage line without crashing the TUI).
pub fn parse_line(line: &str) -> Option<Event> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let mut event: Event = serde_json::from_str(line).ok()?;
    if event.agent_kind.as_deref() == Some("") {
        event.agent_kind = None;
    }
    if event.session_id.as_deref() == Some("") {
        event.session_id = None;
    }
    match event.jjfx_event_version {
        None | Some(1) => Some(event),
        Some(_) => None,
    }
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
        // No hook marks a permission dialog being resolved, but the approved
        // tool completing right after proves the turn resumed - recover from
        // needs-attention only, so a stray tool event cannot wake other states.
        "PostToolUse" | "PostToolUseFailure" if current == AgentState::NeedsAttention => {
            AgentState::Working
        }
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
/// happens in exactly one place. At most one agent runs per workspace (CONTEXT).
/// Session identity prevents delayed records from a replaced session from
/// changing the current agent.
#[derive(Debug, Default)]
struct TrackedAgent {
    agent: Agent,
    session_id: Option<String>,
}

#[derive(Debug, Default)]
pub struct AgentStates {
    states: HashMap<PathBuf, TrackedAgent>,
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
        if ev.name == "SessionStart" {
            // The append-only stream defines session order: each start is the
            // authoritative switch, then IDs reject delayed events from the
            // session it replaced.
            entry.session_id.clone_from(&ev.session_id);
        } else {
            match (&entry.session_id, &ev.session_id) {
                (Some(active), Some(incoming)) if active != incoming => return,
                (Some(_), None) if ev.jjfx_event_version.is_some() => return,
                (None, Some(incoming)) => entry.session_id = Some(incoming.clone()),
                _ => {}
            }
        }

        entry.agent.state = transition(entry.agent.state, &ev.name);
        let kind = match ev.agent_kind.as_deref() {
            Some(name) => AgentKind::from_name(name),
            None => ev
                .transcript_path
                .as_deref()
                .map(AgentKind::from_transcript_path)
                .unwrap_or_default(),
        };
        let starts_versioned_session = ev.name == "SessionStart" && ev.jjfx_event_version.is_some();
        if starts_versioned_session || kind != AgentKind::Unknown {
            entry.agent.kind = kind;
        }
    }

    /// The live agent for a workspace `path`, canonicalized to match the `cwd`
    /// keys so the two sides of the join compare equal. Default (`Absent`,
    /// `Unknown`) if the log has no events for it.
    pub fn agent_for(&self, path: &Path) -> Agent {
        self.states
            .get(&canon(path))
            .map(|tracked| tracked.agent)
            .unwrap_or_default()
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
    fn parses_versioned_lifecycle_envelopes() {
        let line = r#"{"jjfx_event_version":1,"agent_kind":"pi","session_id":"pi-session","cwd":"/w/a","hook_event_name":"SessionStart"}"#;
        let ev = parse_line(line).unwrap();
        assert_eq!(ev.jjfx_event_version, Some(1));
        assert_eq!(ev.agent_kind.as_deref(), Some("pi"));
        assert_eq!(ev.session_id.as_deref(), Some("pi-session"));

        let unsupported = r#"{"jjfx_event_version":2,"agent_kind":"pi","session_id":"pi-session","cwd":"/w/a","hook_event_name":"SessionStart"}"#;
        assert!(parse_line(unsupported).is_none());
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
    fn explicit_identity_selects_pi_without_transcript_fallback() {
        let mut states = AgentStates::default();
        states.apply(
            &parse_line(
                r#"{"jjfx_event_version":1,"agent_kind":"pi","session_id":"pi-1","cwd":"/w/a","hook_event_name":"SessionStart","transcript_path":"/u/.claude/session.jsonl"}"#,
            )
            .unwrap(),
        );

        let agent = states.agent_for(Path::new("/w/a"));
        assert_eq!(agent.kind, AgentKind::Pi);
        assert_eq!(agent.kind.label(), "pi");
    }

    #[test]
    fn new_session_with_missing_identity_resets_to_unknown() {
        let events = [
            r#"{"session_id":"claude-1","transcript_path":"/u/.claude/projects/x/s.jsonl","cwd":"/w/a","hook_event_name":"SessionStart"}"#,
            r#"{"jjfx_event_version":1,"session_id":"unknown-1","cwd":"/w/a","hook_event_name":"SessionStart"}"#,
        ];
        let states = AgentStates::replay(events.into_iter().filter_map(parse_line));

        let agent = states.agent_for(Path::new("/w/a"));
        assert_eq!(agent.state, AgentState::Waiting);
        assert_eq!(agent.kind, AgentKind::Unknown);
    }

    #[test]
    fn fold_keeps_the_last_known_kind() {
        let mut states = AgentStates::default();
        states.apply(&Event {
            name: "SessionStart".to_string(),
            cwd: "/w/a".to_string(),
            transcript_path: Some("/u/.claude/projects/x/s.jsonl".to_string()),
            jjfx_event_version: None,
            agent_kind: None,
            session_id: None,
        });
        assert_eq!(states.agent_for(Path::new("/w/a")).kind, AgentKind::Claude);

        // A field-less line advances the state but cannot wipe the kind.
        states.apply(&Event {
            name: "UserPromptSubmit".to_string(),
            cwd: "/w/a".to_string(),
            transcript_path: None,
            jjfx_event_version: None,
            agent_kind: None,
            session_id: None,
        });
        let agent = states.agent_for(Path::new("/w/a"));
        assert_eq!(agent.state, AgentState::Working);
        assert_eq!(agent.kind, AgentKind::Claude);

        // The workspace switches CLIs: the new agent's first event retags it.
        states.apply(&Event {
            name: "SessionStart".to_string(),
            cwd: "/w/a".to_string(),
            transcript_path: Some("/u/.codex/sessions/r.jsonl".to_string()),
            jjfx_event_version: None,
            agent_kind: None,
            session_id: None,
        });
        assert_eq!(states.agent_for(Path::new("/w/a")).kind, AgentKind::Codex);
    }

    #[test]
    fn stale_events_from_a_replaced_session_do_not_change_state() {
        let lines = [
            r#"{"jjfx_event_version":1,"agent_kind":"pi","session_id":"pi-1","cwd":"/w/a","hook_event_name":"SessionStart"}"#,
            r#"{"jjfx_event_version":1,"agent_kind":"pi","session_id":"pi-1","cwd":"/w/a","hook_event_name":"UserPromptSubmit"}"#,
            r#"{"jjfx_event_version":1,"agent_kind":"pi","session_id":"pi-2","cwd":"/w/a","hook_event_name":"SessionStart"}"#,
            r#"{"jjfx_event_version":1,"agent_kind":"pi","session_id":"pi-2","cwd":"/w/a","hook_event_name":"UserPromptSubmit"}"#,
            r#"{"jjfx_event_version":1,"agent_kind":"pi","session_id":"pi-1","cwd":"/w/a","hook_event_name":"Stop"}"#,
        ];

        let states = AgentStates::replay(lines.into_iter().filter_map(parse_line));
        let agent = states.agent_for(Path::new("/w/a"));
        assert_eq!(agent.state, AgentState::Working);
        assert_eq!(agent.kind, AgentKind::Pi);
    }

    #[test]
    fn partial_versioned_event_cannot_mutate_a_known_session() {
        let lines = [
            r#"{"jjfx_event_version":1,"agent_kind":"pi","session_id":"pi-1","cwd":"/w/a","hook_event_name":"SessionStart"}"#,
            r#"{"jjfx_event_version":1,"agent_kind":"pi","session_id":"pi-1","cwd":"/w/a","hook_event_name":"UserPromptSubmit"}"#,
            r#"{"jjfx_event_version":1,"agent_kind":"pi","cwd":"/w/a","hook_event_name":"Stop"}"#,
        ];

        let states = AgentStates::replay(lines.into_iter().filter_map(parse_line));
        assert_eq!(
            states.agent_for(Path::new("/w/a")).state,
            AgentState::Working
        );
    }

    #[test]
    fn pi_replay_and_live_application_are_equivalent() {
        let lines = [
            r#"{"jjfx_event_version":1,"agent_kind":"pi","session_id":"pi-1","cwd":"/w/a","hook_event_name":"SessionStart"}"#,
            r#"{"jjfx_event_version":1,"agent_kind":"pi","session_id":"pi-1","cwd":"/w/a","hook_event_name":"UserPromptSubmit"}"#,
            r#"{"jjfx_event_version":1,"agent_kind":"pi","session_id":"pi-1","cwd":"/w/a","hook_event_name":"Stop"}"#,
            r#"{"jjfx_event_version":1,"agent_kind":"pi","session_id":"pi-1","cwd":"/w/a","hook_event_name":"SessionEnd"}"#,
        ];
        let events: Vec<_> = lines.into_iter().filter_map(parse_line).collect();
        let replayed = AgentStates::replay(events.clone());
        let mut live = AgentStates::default();
        for event in &events {
            live.apply(event);
        }

        assert_eq!(
            replayed.agent_for(Path::new("/w/a")),
            live.agent_for(Path::new("/w/a"))
        );
        assert_eq!(
            replayed.agent_for(Path::new("/w/a")),
            Agent {
                state: AgentState::Ended,
                kind: AgentKind::Pi,
            }
        );
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
        // A tool completing right after a permission dialog proves the dialog
        // was resolved and the turn resumed...
        assert_eq!(transition(NeedsAttention, "PostToolUse"), Working);
        assert_eq!(transition(NeedsAttention, "PostToolUseFailure"), Working);
        // ...but tool events never wake any other state.
        assert_eq!(transition(Waiting, "PostToolUse"), Waiting);
        assert_eq!(transition(Working, "PostToolUse"), Working);
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
                jjfx_event_version: None,
                agent_kind: None,
                session_id: None,
            });
        }
        assert_eq!(
            states.agent_for(Path::new("/w/a")).state,
            AgentState::Working
        );
    }
}
