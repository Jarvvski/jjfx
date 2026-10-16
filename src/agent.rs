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
    /// The turn ended on a quota or billing wall (`StopFailure:rate_limit` /
    /// `billing_error`): the Run is blocked on provider capacity or account
    /// state, not on a bug, so it must not be retried like an ordinary failure.
    QuotaBlocked,
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
    /// OpenCode coding agent.
    OpenCode,
}

impl AgentKind {
    /// The agent's name for a list row, with a neutral fallback.
    pub fn label(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::OpenCode => "opencode",
            AgentKind::Unknown => "agent",
        }
    }

    /// Parse an explicit jjfx lifecycle-envelope identity.
    fn from_name(name: &str) -> Self {
        match name {
            "claude" => AgentKind::Claude,
            "codex" => AgentKind::Codex,
            "opencode" => AgentKind::OpenCode,
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
    /// Claude `Notification` type (`agent_needs_input`, `idle_prompt`, ...).
    /// The `Notification` hook is a catch-all; this field is the discriminator.
    #[serde(default)]
    pub notification_type: Option<String>,
    /// Claude `StopFailure` error type (`rate_limit`, `billing_error`, ...).
    /// The `StopFailure` hook reports why a turn ended on an API error.
    #[serde(default)]
    pub error: Option<String>,
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

/// The event -> agent-state transition map confirmed in spike 01, extended with
/// the richer 2.x signals (ADR 0002). Events whose meaning depends on a payload
/// field (`Notification`, `StopFailure`) are dispatched first; the rest reduce on
/// the event name alone. Unknown events (the wider 2.x set jjfx does not model)
/// leave the state unchanged.
fn transition(current: AgentState, event: &Event) -> AgentState {
    match event.name.as_str() {
        "Notification" => notification_transition(current, event.notification_type.as_deref()),
        "StopFailure" => stop_failure_transition(event.error.as_deref()),
        name => transition_by_name(current, name),
    }
}

/// The name-only transition map. Kept separate so the payload-free events (and
/// the tests that pin them) read exactly as before.
fn transition_by_name(current: AgentState, event: &str) -> AgentState {
    match event {
        "SessionStart" => AgentState::Waiting,
        "UserPromptSubmit" => AgentState::Working,
        "Stop" | "StopFailure" => AgentState::Waiting,
        "Interrupt" => AgentState::Waiting,
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

/// `Notification` is a catch-all keyed by its notification type. Only an explicit
/// `agent_needs_input` is a genuine blocked-on-a-human signal; the other types
/// (`permission_prompt`, `agent_completed`, `idle_prompt`, ...) are covered by
/// the authoritative `PermissionRequest`/`Stop` events and leave the state alone.
fn notification_transition(current: AgentState, notification_type: Option<&str>) -> AgentState {
    match notification_type {
        Some("agent_needs_input") => AgentState::NeedsAttention,
        _ => current,
    }
}

/// A `StopFailure` ends the turn on an API error. A quota or billing error is a
/// distinct, non-retryable outcome: the Run is blocked on provider capacity or
/// account state, so it must not be retried like an ordinary failure. Any other
/// error (or none) is an ordinary finished turn, matching the original
/// `StopFailure -> Waiting` mapping.
fn stop_failure_transition(error: Option<&str>) -> AgentState {
    match error {
        Some("rate_limit" | "billing_error") => AgentState::QuotaBlocked,
        _ => AgentState::Waiting,
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
    /// Workspace path -> canonical join key, memoised so a caller that asks for
    /// the same workspace every frame or tick does not `canonicalize` it again
    /// (that syscall dominated the idle render path). Populated by
    /// [`track`](Self::track) when the workspace set changes; a path that moved
    /// is simply a new key, so a stale entry can never mis-join.
    keys: HashMap<PathBuf, PathBuf>,
}

impl AgentStates {
    /// Remember `path`'s canonical join key, so later
    /// [`agent_for`](Self::agent_for) lookups skip the `canonicalize` syscall.
    /// Re-resolved on each call - the TUI calls it when the workspace set
    /// changes, which is what picks up a path that moved - while the per-frame
    /// lookup stays a hash probe.
    pub fn track(&mut self, path: &Path) {
        self.keys.insert(path.to_path_buf(), canon(path));
    }

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

        entry.agent.state = transition(entry.agent.state, ev);
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
    /// `Unknown`) if the log has no events for it. A [`track`](Self::track)ed
    /// path reuses its memoised key, so a per-frame lookup is a hash probe
    /// rather than a syscall.
    pub fn agent_for(&self, path: &Path) -> Agent {
        let key = self.keys.get(path).cloned().unwrap_or_else(|| canon(path));
        self.states
            .get(&key)
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
        let line = r#"{"jjfx_event_version":1,"agent_kind":"opencode","session_id":"oc-session","cwd":"/w/a","hook_event_name":"SessionStart"}"#;
        let ev = parse_line(line).unwrap();
        assert_eq!(ev.jjfx_event_version, Some(1));
        assert_eq!(ev.agent_kind.as_deref(), Some("opencode"));
        assert_eq!(ev.session_id.as_deref(), Some("oc-session"));

        let unsupported = r#"{"jjfx_event_version":2,"agent_kind":"opencode","session_id":"oc-session","cwd":"/w/a","hook_event_name":"SessionStart"}"#;
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
    fn explicit_identity_selects_opencode_without_transcript_fallback() {
        let mut states = AgentStates::default();
        states.apply(
            &parse_line(
                r#"{"jjfx_event_version":1,"agent_kind":"opencode","session_id":"oc-1","cwd":"/w/a","hook_event_name":"SessionStart","transcript_path":"/u/.claude/session.jsonl"}"#,
            )
            .unwrap(),
        );

        let agent = states.agent_for(Path::new("/w/a"));
        assert_eq!(agent.kind, AgentKind::OpenCode);
        assert_eq!(agent.kind.label(), "opencode");
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
            notification_type: None,
            error: None,
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
            notification_type: None,
            error: None,
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
            notification_type: None,
            error: None,
        });
        assert_eq!(states.agent_for(Path::new("/w/a")).kind, AgentKind::Codex);
    }

    #[test]
    fn stale_events_from_a_replaced_session_do_not_change_state() {
        let lines = [
            r#"{"jjfx_event_version":1,"agent_kind":"opencode","session_id":"oc-1","cwd":"/w/a","hook_event_name":"SessionStart"}"#,
            r#"{"jjfx_event_version":1,"agent_kind":"opencode","session_id":"oc-1","cwd":"/w/a","hook_event_name":"UserPromptSubmit"}"#,
            r#"{"jjfx_event_version":1,"agent_kind":"opencode","session_id":"oc-2","cwd":"/w/a","hook_event_name":"SessionStart"}"#,
            r#"{"jjfx_event_version":1,"agent_kind":"opencode","session_id":"oc-2","cwd":"/w/a","hook_event_name":"UserPromptSubmit"}"#,
            r#"{"jjfx_event_version":1,"agent_kind":"opencode","session_id":"oc-1","cwd":"/w/a","hook_event_name":"Stop"}"#,
        ];

        let states = AgentStates::replay(lines.into_iter().filter_map(parse_line));
        let agent = states.agent_for(Path::new("/w/a"));
        assert_eq!(agent.state, AgentState::Working);
        assert_eq!(agent.kind, AgentKind::OpenCode);
    }

    #[test]
    fn partial_versioned_event_cannot_mutate_a_known_session() {
        let lines = [
            r#"{"jjfx_event_version":1,"agent_kind":"opencode","session_id":"oc-1","cwd":"/w/a","hook_event_name":"SessionStart"}"#,
            r#"{"jjfx_event_version":1,"agent_kind":"opencode","session_id":"oc-1","cwd":"/w/a","hook_event_name":"UserPromptSubmit"}"#,
            r#"{"jjfx_event_version":1,"agent_kind":"opencode","cwd":"/w/a","hook_event_name":"Stop"}"#,
        ];

        let states = AgentStates::replay(lines.into_iter().filter_map(parse_line));
        assert_eq!(
            states.agent_for(Path::new("/w/a")).state,
            AgentState::Working
        );
    }

    #[test]
    fn opencode_replay_and_live_application_are_equivalent() {
        let lines = [
            r#"{"jjfx_event_version":1,"agent_kind":"opencode","session_id":"oc-1","cwd":"/w/a","hook_event_name":"SessionStart"}"#,
            r#"{"jjfx_event_version":1,"agent_kind":"opencode","session_id":"oc-1","cwd":"/w/a","hook_event_name":"UserPromptSubmit"}"#,
            r#"{"jjfx_event_version":1,"agent_kind":"opencode","session_id":"oc-1","cwd":"/w/a","hook_event_name":"Stop"}"#,
            r#"{"jjfx_event_version":1,"agent_kind":"opencode","session_id":"oc-1","cwd":"/w/a","hook_event_name":"SessionEnd"}"#,
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
                kind: AgentKind::OpenCode,
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
        assert_eq!(transition_by_name(Absent, "SessionStart"), Waiting);
        assert_eq!(transition_by_name(Waiting, "UserPromptSubmit"), Working);
        assert_eq!(transition_by_name(Working, "Stop"), Waiting);
        assert_eq!(transition_by_name(Working, "StopFailure"), Waiting);
        assert_eq!(
            transition_by_name(Waiting, "PermissionRequest"),
            NeedsAttention
        );
        assert_eq!(transition_by_name(Working, "SessionEnd"), Ended);
        // A tool completing right after a permission dialog proves the dialog
        // was resolved and the turn resumed...
        assert_eq!(transition_by_name(NeedsAttention, "PostToolUse"), Working);
        assert_eq!(
            transition_by_name(NeedsAttention, "PostToolUseFailure"),
            Working
        );
        // ...but tool events never wake any other state.
        assert_eq!(transition_by_name(Waiting, "PostToolUse"), Waiting);
        assert_eq!(transition_by_name(Working, "PostToolUse"), Working);
        // An event jjfx does not model leaves the state untouched.
        assert_eq!(transition_by_name(Working, "PreToolUse"), Working);
    }

    /// Parse a legacy Claude payload line and fold it into a fresh state, so the
    /// transition is exercised through the same `parse_line` + `apply` seam the
    /// tail uses rather than a hand-built `Event`.
    fn fold(lines: &[&str]) -> Agent {
        let states = AgentStates::replay(lines.iter().filter_map(|line| parse_line(line)));
        states.agent_for(Path::new("/w/a"))
    }

    #[test]
    fn notification_agent_needs_input_is_needs_attention() {
        // The `Notification` catch-all is discriminated by `notification_type`,
        // not the message text: only `agent_needs_input` is a real needs-you.
        assert_eq!(
            fold(&[
                r#"{"cwd":"/w/a","hook_event_name":"SessionStart"}"#,
                r#"{"cwd":"/w/a","hook_event_name":"UserPromptSubmit"}"#,
                r#"{"cwd":"/w/a","hook_event_name":"Notification","notification_type":"agent_needs_input","message":"Claude needs your input"}"#,
            ])
            .state,
            AgentState::NeedsAttention
        );
    }

    #[test]
    fn other_notification_types_do_not_disturb_the_state() {
        // `agent_completed` / `idle_prompt` / `permission_prompt` are covered by
        // the authoritative events, so they must not move the fold.
        for notification_type in ["agent_completed", "idle_prompt", "permission_prompt"] {
            let line = format!(
                r#"{{"cwd":"/w/a","hook_event_name":"Notification","notification_type":"{notification_type}"}}"#
            );
            let agent = fold(&[
                r#"{"cwd":"/w/a","hook_event_name":"SessionStart"}"#,
                r#"{"cwd":"/w/a","hook_event_name":"UserPromptSubmit"}"#,
                &line,
            ]);
            assert_eq!(
                agent.state,
                AgentState::Working,
                "{notification_type} must leave Working untouched"
            );
        }
    }

    #[test]
    fn quota_stop_failure_is_distinct_from_a_plain_failure() {
        // A rate-limit/billing StopFailure is a quota wall, not a finished turn.
        for error in ["rate_limit", "billing_error"] {
            let line = format!(
                r#"{{"cwd":"/w/a","hook_event_name":"StopFailure","error":"{error}","last_assistant_message":"API Error"}}"#
            );
            let agent = fold(&[
                r#"{"cwd":"/w/a","hook_event_name":"SessionStart"}"#,
                r#"{"cwd":"/w/a","hook_event_name":"UserPromptSubmit"}"#,
                &line,
            ]);
            assert_eq!(
                agent.state,
                AgentState::QuotaBlocked,
                "{error} must be a quota block"
            );
        }

        // Any other StopFailure error stays an ordinary finished turn, and a
        // payload without an error keeps the original mapping.
        assert_eq!(
            fold(&[
                r#"{"cwd":"/w/a","hook_event_name":"SessionStart"}"#,
                r#"{"cwd":"/w/a","hook_event_name":"UserPromptSubmit"}"#,
                r#"{"cwd":"/w/a","hook_event_name":"StopFailure","error":"server_error"}"#,
            ])
            .state,
            AgentState::Waiting
        );
        assert_eq!(
            fold(&[
                r#"{"cwd":"/w/a","hook_event_name":"SessionStart"}"#,
                r#"{"cwd":"/w/a","hook_event_name":"UserPromptSubmit"}"#,
                r#"{"cwd":"/w/a","hook_event_name":"StopFailure"}"#,
            ])
            .state,
            AgentState::Waiting
        );
    }

    #[test]
    fn quota_block_recovers_on_the_next_turn_or_stop() {
        let quota = [
            r#"{"cwd":"/w/a","hook_event_name":"SessionStart"}"#,
            r#"{"cwd":"/w/a","hook_event_name":"UserPromptSubmit"}"#,
            r#"{"cwd":"/w/a","hook_event_name":"StopFailure","error":"rate_limit"}"#,
        ];
        assert_eq!(fold(&quota).state, AgentState::QuotaBlocked);

        // A fresh prompt once the quota resets returns the agent to Working...
        let mut resumed = quota.to_vec();
        resumed.push(r#"{"cwd":"/w/a","hook_event_name":"UserPromptSubmit"}"#);
        assert_eq!(fold(&resumed).state, AgentState::Working);

        // ...and a plain Stop settles it back to Waiting.
        let mut stopped = quota.to_vec();
        stopped.push(r#"{"cwd":"/w/a","hook_event_name":"Stop"}"#);
        assert_eq!(fold(&stopped).state, AgentState::Waiting);
    }

    #[test]
    fn subagent_and_batch_events_are_recorded_without_regressing_the_turn() {
        // SubagentStart/Stop, PostToolBatch, TaskCreated/Completed, and
        // TeammateIdle are carried in the log for progress, but the fold must not
        // regress a Working or Waiting turn.
        let working = [
            r#"{"cwd":"/w/a","hook_event_name":"SessionStart"}"#,
            r#"{"cwd":"/w/a","hook_event_name":"UserPromptSubmit"}"#,
            r#"{"cwd":"/w/a","hook_event_name":"SubagentStart","agent_type":"Explore"}"#,
            r#"{"cwd":"/w/a","hook_event_name":"PostToolBatch"}"#,
            r#"{"cwd":"/w/a","hook_event_name":"TaskCreated"}"#,
            r#"{"cwd":"/w/a","hook_event_name":"TaskCompleted"}"#,
            r#"{"cwd":"/w/a","hook_event_name":"SubagentStop","agent_type":"Explore"}"#,
            r#"{"cwd":"/w/a","hook_event_name":"TeammateIdle"}"#,
        ];
        assert_eq!(fold(&working).state, AgentState::Working);

        let mut waiting = working.to_vec();
        waiting.push(r#"{"cwd":"/w/a","hook_event_name":"Stop"}"#);
        waiting.push(r#"{"cwd":"/w/a","hook_event_name":"SubagentStart"}"#);
        waiting.push(r#"{"cwd":"/w/a","hook_event_name":"PostToolBatch"}"#);
        assert_eq!(fold(&waiting).state, AgentState::Waiting);
    }

    #[test]
    fn codex_interrupt_settles_to_waiting() {
        assert_eq!(
            fold(&[
                r#"{"cwd":"/w/a","hook_event_name":"SessionStart"}"#,
                r#"{"cwd":"/w/a","hook_event_name":"UserPromptSubmit"}"#,
                r#"{"cwd":"/w/a","hook_event_name":"Interrupt"}"#,
            ])
            .state,
            AgentState::Waiting
        );
    }

    #[test]
    fn legacy_lines_without_the_new_fields_still_parse() {
        // Backward compatibility: an events.jsonl line written before the schema
        // extension has neither `notification_type` nor `error`, and must fold
        // exactly as it always did.
        let agent = fold(&[
            r#"{"cwd":"/w/a","hook_event_name":"SessionStart"}"#,
            r#"{"cwd":"/w/a","hook_event_name":"UserPromptSubmit"}"#,
            r#"{"cwd":"/w/a","hook_event_name":"Stop"}"#,
        ]);
        assert_eq!(agent.state, AgentState::Waiting);
        assert_eq!(agent.kind, AgentKind::Unknown);
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
    fn a_tracked_path_reuses_its_canonical_key() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let mut states = AgentStates::default();
        states.track(&link);
        states.apply(&Event {
            name: "UserPromptSubmit".to_string(),
            cwd: real.to_string_lossy().into_owned(),
            transcript_path: None,
            jjfx_event_version: None,
            agent_kind: None,
            session_id: None,
            notification_type: None,
            error: None,
        });
        assert_eq!(states.agent_for(&link).state, AgentState::Working);

        // The key is resolved once: retargeting the link does not re-resolve it
        // under the TUI's feet, so the join stays on the workspace it saw.
        let other = tmp.path().join("other");
        std::fs::create_dir(&other).unwrap();
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&other, &link).unwrap();
        assert_eq!(states.agent_for(&link).state, AgentState::Working);

        // Re-tracking (what a workspace-set change does) picks up the move.
        states.track(&link);
        assert_eq!(states.agent_for(&link).state, AgentState::Absent);
    }

    #[test]
    fn tracking_a_path_leaves_untracked_lookups_correct() {
        let mut states = AgentStates::default();
        states.track(Path::new("/w/tracked"));
        states.apply(&Event {
            name: "UserPromptSubmit".to_string(),
            cwd: "/w/untracked".to_string(),
            transcript_path: None,
            jjfx_event_version: None,
            agent_kind: None,
            session_id: None,
            notification_type: None,
            error: None,
        });
        // An untracked path still resolves through the fallback canonicalisation.
        assert_eq!(
            states.agent_for(Path::new("/w/untracked")).state,
            AgentState::Working
        );
        assert_eq!(
            states.agent_for(Path::new("/w/tracked")).state,
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
                notification_type: None,
                error: None,
            });
        }
        assert_eq!(
            states.agent_for(Path::new("/w/a")).state,
            AgentState::Working
        );
    }
}
