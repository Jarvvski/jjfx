//! Provider-neutral parsing and values for structured Agent Runtime logs.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Number, Value};
use thiserror::Error;

use crate::runtime::AgentRuntime;

const ACTIVITY_TAIL_BYTES: u64 = 65_536;

/// The result of resolving a prior Agent Session from a structured Run log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentSessionResolution {
    /// The prior Agent Session can be resumed.
    Resumed {
        /// Provider-reported Agent Session identity.
        session_id: String,
    },
    /// A Follow-up must start a fresh Agent Session.
    Fresh {
        /// Why the prior Agent Session could not be resumed.
        reason: FreshSessionReason,
    },
}

/// Why a Follow-up must start a fresh Agent Session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreshSessionReason {
    /// The Worker has no prior Run log.
    NoPriorLog,
    /// The prior Run log could not be read.
    UnreadableLog {
        /// Path that could not be read.
        path: PathBuf,
        /// Filesystem failure detail.
        detail: String,
    },
    /// The prior Run log does not contain a provider Session identity.
    MissingIdentity,
}

impl fmt::Display for FreshSessionReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPriorLog => formatter.write_str("no prior session log"),
            Self::UnreadableLog { path, detail } => {
                write!(
                    formatter,
                    "log file unreadable ({}: {detail})",
                    path.display()
                )
            }
            Self::MissingIdentity => formatter.write_str("log has no session id yet"),
        }
    }
}

/// Resolves whether a Follow-up can resume the Agent Session in `prior_log`.
pub fn resolve_agent_session(prior_log: Option<&Path>) -> AgentSessionResolution {
    let Some(path) = prior_log.filter(|path| !path.as_os_str().is_empty()) else {
        return AgentSessionResolution::Fresh {
            reason: FreshSessionReason::NoPriorLog,
        };
    };
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(source) => return unreadable_agent_session_log(path, source),
    };
    let mut reader = BufReader::new(file);
    let mut record = Vec::new();

    loop {
        record.clear();
        match reader.read_until(b'\n', &mut record) {
            Ok(0) => break,
            Ok(_) => {}
            Err(source) => return unreadable_agent_session_log(path, source),
        }
        let Ok(event) = serde_json::from_slice::<AgentSessionEvent>(&record) else {
            continue;
        };
        let session_id = match event.event_type.as_str() {
            "system" if event.subtype == "init" => event.session_id,
            "thread.started" => event.thread_id,
            _ => continue,
        };
        if !session_id.is_empty() {
            return AgentSessionResolution::Resumed { session_id };
        }
    }

    AgentSessionResolution::Fresh {
        reason: FreshSessionReason::MissingIdentity,
    }
}

fn unreadable_agent_session_log(path: &Path, source: std::io::Error) -> AgentSessionResolution {
    AgentSessionResolution::Fresh {
        reason: FreshSessionReason::UnreadableLog {
            path: path.to_path_buf(),
            detail: source.to_string(),
        },
    }
}

#[derive(Debug, Deserialize)]
struct AgentSessionEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    subtype: String,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    thread_id: String,
}

/// One structured Agent Runtime log.
#[derive(Debug)]
pub struct RunLog {
    path: PathBuf,
    runtime: AgentRuntime,
}

impl RunLog {
    /// Opens a provider log through the shared Run log interface.
    pub fn new(path: impl AsRef<Path>, runtime: AgentRuntime) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            runtime,
        }
    }

    /// Returns the latest provider-neutral activity in the final 64 KiB of the log.
    pub fn current_activity(&self) -> Result<Option<RunActivity>, RunLogError> {
        let mut file = fs::File::open(&self.path).map_err(|source| self.io_error(source))?;
        let length = file
            .metadata()
            .map_err(|source| self.io_error(source))?
            .len();
        let tail_start = length.saturating_sub(ACTIVITY_TAIL_BYTES);
        let read_start = tail_start.saturating_sub(1);
        file.seek(SeekFrom::Start(read_start))
            .map_err(|source| self.io_error(source))?;

        let read_length = length.saturating_sub(read_start);
        let mut contents = Vec::with_capacity(read_length as usize);
        file.take(read_length)
            .read_to_end(&mut contents)
            .map_err(|source| self.io_error(source))?;

        let contents = if tail_start == 0 {
            contents.as_slice()
        } else if contents.first() == Some(&b'\n') {
            &contents[1..]
        } else {
            contents
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(&[][..], |line_end| &contents[line_end + 1..])
        };

        let mut parser = RunLogParser::new(self.runtime);
        let mut latest = None;
        for record in contents.split(|byte| *byte == b'\n') {
            for event in self.parse_record(&mut parser, record)? {
                if let RunLogEvent::Activity(activity) = event {
                    latest = Some(activity);
                }
            }
        }

        Ok(latest)
    }

    /// Returns the latest terminal result after scanning the complete log.
    pub fn final_result(&self) -> Result<Option<RunResult>, RunLogError> {
        let file = fs::File::open(&self.path).map_err(|source| self.io_error(source))?;
        let mut reader = BufReader::new(file);
        let mut parser = RunLogParser::new(self.runtime);
        let mut buffer = Vec::new();
        let mut latest = None;

        loop {
            buffer.clear();
            if reader
                .read_until(b'\n', &mut buffer)
                .map_err(|source| self.io_error(source))?
                == 0
            {
                break;
            }
            for event in self.parse_record(&mut parser, &buffer)? {
                if let RunLogEvent::Result(result) = event {
                    latest = Some(result);
                }
            }
        }

        Ok(latest)
    }

    fn parse_record(
        &self,
        parser: &mut RunLogParser,
        record: &[u8],
    ) -> Result<Vec<RunLogEvent>, RunLogError> {
        let Ok(line) = str::from_utf8(record) else {
            return Ok(Vec::new());
        };
        match parser.parse_line(line) {
            Ok(events) => Ok(events),
            Err(RunLogParseError::InvalidEvent { .. }) => Ok(Vec::new()),
            Err(source) => Err(RunLogError::Parse {
                path: self.path.clone(),
                source,
            }),
        }
    }

    fn io_error(&self, source: std::io::Error) -> RunLogError {
        RunLogError::Io {
            path: self.path.clone(),
            source,
        }
    }
}

/// Failure to read or semantically decode a structured Run log.
#[derive(Debug, Error)]
pub enum RunLogError {
    /// The Run log could not be read.
    #[error("failed to read Run log {path}: {source}")]
    Io {
        /// Path to the Run log.
        path: PathBuf,
        /// File access failure.
        #[source]
        source: std::io::Error,
    },
    /// A provider event was syntactically valid but semantically unsupported.
    #[error("failed to decode Run log {path}: {source}")]
    Parse {
        /// Path to the Run log.
        path: PathBuf,
        /// Provider event decoding failure.
        #[source]
        source: RunLogParseError,
    },
}

/// Where the terminal result used to finalize a Run came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunResultSource {
    /// The Agent Runtime emitted a recognized terminal event.
    Provider,
    /// No compatible terminal result was available, so finalization used the
    /// compatible unexpected-exit failure.
    Fallback {
        /// Why the provider result could not be used.
        reason: RunResultFallback,
    },
}

/// Why Run finalization had to use the compatible unexpected-exit result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunResultFallback {
    /// The readable log contained no recognized terminal event.
    MissingTerminalEvent,
    /// The configured log could not be read.
    UnreadableLog { detail: String },
    /// A terminal event could not be normalized safely.
    InvalidTerminalEvent { detail: String },
}

pub(crate) fn result_for_finalization(
    path: &Path,
    runtime: AgentRuntime,
) -> (RunResult, RunResultSource) {
    match RunLog::new(path, runtime).final_result() {
        Ok(Some(result)) => (result, RunResultSource::Provider),
        Ok(None) => unexpected_result(RunResultFallback::MissingTerminalEvent),
        Err(RunLogError::Io { source, .. }) => {
            unexpected_result(RunResultFallback::UnreadableLog {
                detail: source.to_string(),
            })
        }
        Err(RunLogError::Parse { source, .. }) => {
            unexpected_result(RunResultFallback::InvalidTerminalEvent {
                detail: source.to_string(),
            })
        }
    }
}

pub(crate) fn unexpected_result(reason: RunResultFallback) -> (RunResult, RunResultSource) {
    (
        RunResult::failed("Process exited unexpectedly"),
        RunResultSource::Fallback { reason },
    )
}

/// One provider-neutral value decoded from an Agent Runtime log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunLogEvent {
    /// One meaningful activity reported during a Run.
    Activity(RunActivity),
    /// The Agent Runtime's terminal Run result.
    Result(RunResult),
}

/// Parses provider log lines without exposing provider event shapes.
#[derive(Debug)]
pub struct RunLogParser {
    runtime: AgentRuntime,
    claude_tools: HashMap<String, ClaudeToolCall>,
}

impl RunLogParser {
    /// Creates a parser for one Agent Runtime's log protocol.
    pub fn new(runtime: AgentRuntime) -> Self {
        Self {
            runtime,
            claude_tools: HashMap::new(),
        }
    }

    /// Parses one complete log line into zero or more provider-neutral events.
    pub fn parse_line(&mut self, line: &str) -> Result<Vec<RunLogEvent>, RunLogParseError> {
        match self.runtime {
            AgentRuntime::Claude => parse_claude_line(line, &mut self.claude_tools),
            AgentRuntime::Codex => parse_codex_line(line),
        }
    }
}

/// Failure to decode a provider log line.
#[derive(Debug, Error)]
pub enum RunLogParseError {
    /// The selected provider adapter has not been implemented yet.
    #[error("Run log parsing is not implemented for {runtime:?}")]
    UnsupportedRuntime {
        /// Runtime whose log protocol is not implemented.
        runtime: AgentRuntime,
    },
    /// A log line was not valid provider JSON.
    #[error("invalid {runtime:?} Run log event: {source}")]
    InvalidEvent {
        /// Runtime whose event failed to decode.
        runtime: AgentRuntime,
        /// Provider JSON decoding failure.
        #[source]
        source: serde_json::Error,
    },
    /// A provider reported a cost outside the supported non-negative range.
    #[error("invalid {runtime:?} Run cost: {value}")]
    InvalidCost {
        /// Runtime that reported the invalid cost.
        runtime: AgentRuntime,
        /// Provider value that could not be represented in micro-USD.
        value: String,
    },
}

#[derive(Debug, Deserialize)]
struct ClaudeEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    subtype: String,
    message: Option<ClaudeMessage>,
    tool: Option<ClaudeToolResult>,
    duration_ms: Option<u64>,
    num_turns: Option<u64>,
    total_cost_usd: Option<Number>,
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    result: String,
}

#[derive(Debug, Deserialize)]
struct ClaudeMessage {
    #[serde(default)]
    content: Vec<ClaudeContent>,
    usage: Option<ClaudeUsage>,
}

#[derive(Debug, Deserialize)]
struct ClaudeToolResult {
    #[serde(default)]
    tool_use_id: String,
}

#[derive(Debug, Deserialize)]
struct ClaudeContent {
    #[serde(rename = "type")]
    content_type: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    tool_use_id: String,
    #[serde(default)]
    input: Value,
    #[serde(default)]
    content: Value,
    #[serde(default)]
    is_error: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct ClaudeUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

impl ClaudeUsage {
    fn normalized(&self) -> RunUsage {
        RunUsage::new(self.input_tokens, self.output_tokens)
            .with_cached_input_tokens(self.cache_read_input_tokens)
            .with_cache_write_input_tokens(self.cache_creation_input_tokens)
    }
}

#[derive(Debug)]
struct ClaudeToolCall {
    name: String,
    detail: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CodexEvent {
    #[serde(rename = "type")]
    event_type: String,
    item: Option<CodexItem>,
    usage: Option<CodexUsage>,
    error: Option<CodexError>,
    #[serde(default)]
    message: String,
}

#[derive(Debug, Deserialize)]
struct CodexItem {
    #[serde(rename = "type")]
    item_type: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    command: String,
    #[serde(default)]
    aggregated_output: String,
    exit_code: Option<i32>,
    #[serde(default)]
    status: String,
    #[serde(default)]
    server: String,
    #[serde(default)]
    tool: String,
    #[serde(default)]
    query: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    changes: Vec<CodexFileChange>,
    #[serde(default)]
    items: Vec<CodexTodoItem>,
    #[serde(default)]
    sender_thread_id: String,
    #[serde(default)]
    receiver_thread_ids: Vec<String>,
    prompt: Option<String>,
    #[serde(default)]
    agents_states: BTreeMap<String, CodexAgentState>,
    error: Option<CodexError>,
}

#[derive(Debug, Deserialize)]
struct CodexFileChange {
    path: String,
}

#[derive(Debug, Deserialize)]
struct CodexTodoItem {
    #[serde(default)]
    completed: bool,
}

#[derive(Debug, Deserialize)]
struct CodexAgentState {
    status: String,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CodexUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
    #[serde(default)]
    cache_write_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    reasoning_output_tokens: u64,
}

impl CodexUsage {
    fn normalized(self) -> RunUsage {
        RunUsage::new(self.input_tokens, self.output_tokens)
            .with_cached_input_tokens(self.cached_input_tokens)
            .with_cache_write_input_tokens(self.cache_write_input_tokens)
            .with_reasoning_output_tokens(self.reasoning_output_tokens)
    }
}

#[derive(Debug, Deserialize)]
struct CodexError {
    #[serde(default)]
    message: String,
}

fn parse_codex_line(line: &str) -> Result<Vec<RunLogEvent>, RunLogParseError> {
    let event: CodexEvent =
        serde_json::from_str(line).map_err(|source| RunLogParseError::InvalidEvent {
            runtime: AgentRuntime::Codex,
            source,
        })?;

    match event.event_type.as_str() {
        "thread.started" => Ok(vec![RunLogEvent::Activity(RunActivity::new(
            RunActivityKind::SessionStarted,
        ))]),
        "turn.completed" => {
            let result = match event.usage {
                Some(usage) => RunResult::succeeded().with_usage(usage.normalized()),
                None => RunResult::succeeded(),
            };
            Ok(vec![RunLogEvent::Result(result)])
        }
        "turn.failed" => Ok(vec![RunLogEvent::Result(RunResult::failed(
            codex_failure_message("turn.failed", event.error, event.message),
        ))]),
        "error" => Ok(vec![RunLogEvent::Result(RunResult::failed(
            codex_failure_message("error", event.error, event.message),
        ))]),
        "item.started" | "item.updated" | "item.completed" => Ok(event
            .item
            .and_then(|item| parse_codex_item(&event.event_type, item))
            .map(RunActivity::new)
            .map(RunLogEvent::Activity)
            .into_iter()
            .collect()),
        _ => Ok(Vec::new()),
    }
}

fn codex_failure_message(event_type: &str, error: Option<CodexError>, message: String) -> String {
    error
        .and_then(|error| non_empty(error.message))
        .or_else(|| non_empty(message))
        .unwrap_or_else(|| event_type.to_owned())
}

fn parse_codex_item(event_type: &str, item: CodexItem) -> Option<RunActivityKind> {
    match item.item_type.as_str() {
        "agent_message" if !item.text.is_empty() => {
            Some(RunActivityKind::Message { text: item.text })
        }
        "reasoning" if !item.text.is_empty() => {
            Some(RunActivityKind::Reasoning { text: item.text })
        }
        "command_execution" if !item.command.is_empty() => Some(RunActivityKind::Tool {
            name: "Command".to_owned(),
            detail: Some(item.command),
            status: parse_codex_status(
                &item.status,
                non_empty(item.aggregated_output)
                    .or_else(|| item.exit_code.map(|code| format!("exit code {code}"))),
            )?,
        }),
        "mcp_tool_call" => {
            let name = format!("{}.{}", item.server, item.tool)
                .trim_matches('.')
                .to_owned();
            if name.is_empty() {
                return None;
            }
            Some(RunActivityKind::Tool {
                name,
                detail: None,
                status: parse_codex_status(
                    &item.status,
                    item.error.and_then(|error| non_empty(error.message)),
                )?,
            })
        }
        "web_search" if !item.query.is_empty() => Some(RunActivityKind::Tool {
            name: "WebSearch".to_owned(),
            detail: Some(item.query),
            status: parse_codex_lifecycle_status(event_type)?,
        }),
        "file_change" if event_type == "item.completed" => Some(RunActivityKind::FileChanges {
            paths: item.changes.into_iter().map(|change| change.path).collect(),
        }),
        "todo_list" => Some(RunActivityKind::Plan {
            completed: item.items.iter().filter(|item| item.completed).count(),
            total: item.items.len(),
        }),
        "error" if event_type == "item.completed" && !item.message.is_empty() => {
            Some(RunActivityKind::Warning {
                message: item.message,
            })
        }
        "collab_tool_call" if !item.tool.is_empty() => Some(RunActivityKind::Collaboration(
            parse_codex_collaboration(item)?,
        )),
        _ => None,
    }
}

fn parse_codex_collaboration(item: CodexItem) -> Option<CollaborationEvent> {
    let failure_message = item.error.and_then(|error| non_empty(error.message));
    let mut collaboration = CollaborationEvent::new(
        item.tool,
        parse_codex_status(&item.status, failure_message)?,
    )
    .with_receivers(item.receiver_thread_ids);
    if !item.sender_thread_id.is_empty() {
        collaboration = collaboration.with_sender(item.sender_thread_id);
    }
    if let Some(prompt) = item.prompt.and_then(|prompt| normalize_whitespace(&prompt)) {
        collaboration = collaboration.with_prompt(prompt);
    }
    for (id, state) in item.agents_states {
        let mut participant = CollaborationParticipant::new(id, state.status);
        if let Some(message) = state
            .message
            .and_then(|message| normalize_whitespace(&message))
        {
            participant = participant.with_message(message);
        }
        collaboration = collaboration.with_participant(participant);
    }
    Some(collaboration)
}

fn normalize_whitespace(value: &str) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

fn parse_codex_lifecycle_status(event_type: &str) -> Option<RunActivityStatus> {
    match event_type {
        "item.started" | "item.updated" => Some(RunActivityStatus::InProgress),
        "item.completed" => Some(RunActivityStatus::Completed),
        _ => None,
    }
}

fn parse_codex_status(status: &str, failure_message: Option<String>) -> Option<RunActivityStatus> {
    match status {
        "in_progress" => Some(RunActivityStatus::InProgress),
        "completed" => Some(RunActivityStatus::Completed),
        "failed" => Some(RunActivityStatus::Failed {
            message: failure_message,
        }),
        "declined" => Some(RunActivityStatus::Declined),
        _ => None,
    }
}

fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn parse_claude_line(
    line: &str,
    tools: &mut HashMap<String, ClaudeToolCall>,
) -> Result<Vec<RunLogEvent>, RunLogParseError> {
    let event: ClaudeEvent =
        serde_json::from_str(line).map_err(|source| RunLogParseError::InvalidEvent {
            runtime: AgentRuntime::Claude,
            source,
        })?;

    match (event.event_type.as_str(), event.subtype.as_str()) {
        ("system", "init") => Ok(vec![RunLogEvent::Activity(RunActivity::new(
            RunActivityKind::SessionStarted,
        ))]),
        ("assistant", _) => Ok(parse_claude_message(event.message, tools, false)),
        ("user", _) => Ok(parse_claude_message(event.message, tools, true)),
        ("tool", _) => Ok(event
            .tool
            .and_then(|tool| complete_claude_tool(tools, &tool.tool_use_id, false, None))
            .map(RunActivity::new)
            .map(RunLogEvent::Activity)
            .into_iter()
            .collect()),
        ("result", _) => parse_claude_result(event).map(|result| vec![result]),
        _ => Ok(Vec::new()),
    }
}

fn parse_claude_message(
    message: Option<ClaudeMessage>,
    tools: &mut HashMap<String, ClaudeToolCall>,
    is_user: bool,
) -> Vec<RunLogEvent> {
    let Some(message) = message else {
        return Vec::new();
    };
    let usage = message.usage.map(|usage| usage.normalized());
    let mut events = Vec::new();

    for content in message.content {
        let kind = match content.content_type.as_str() {
            "text" if !is_user && !content.text.is_empty() => {
                Some(RunActivityKind::Message { text: content.text })
            }
            "tool_use" if !is_user && !content.name.is_empty() => {
                let detail = summarize_claude_input(&content.input);
                if !content.id.is_empty() {
                    tools.insert(
                        content.id,
                        ClaudeToolCall {
                            name: content.name.clone(),
                            detail: detail.clone(),
                        },
                    );
                }
                Some(RunActivityKind::Tool {
                    name: content.name,
                    detail,
                    status: RunActivityStatus::InProgress,
                })
            }
            "tool_result" if is_user => {
                let message = content
                    .content
                    .as_str()
                    .filter(|message| !message.is_empty())
                    .map(ToOwned::to_owned);
                complete_claude_tool(tools, &content.tool_use_id, content.is_error, message)
            }
            _ => None,
        };

        if let Some(kind) = kind {
            let activity = RunActivity::new(kind);
            events.push(RunLogEvent::Activity(match &usage {
                Some(usage) => activity.with_usage(usage.clone()),
                None => activity,
            }));
        }
    }

    events
}

fn complete_claude_tool(
    tools: &mut HashMap<String, ClaudeToolCall>,
    tool_use_id: &str,
    is_error: bool,
    message: Option<String>,
) -> Option<RunActivityKind> {
    let tool = tools.remove(tool_use_id)?;
    Some(RunActivityKind::Tool {
        name: tool.name,
        detail: tool.detail,
        status: if is_error {
            RunActivityStatus::Failed { message }
        } else {
            RunActivityStatus::Completed
        },
    })
}

fn summarize_claude_input(input: &Value) -> Option<String> {
    let fields = input.as_object()?;
    ["command", "file_path", "description", "pattern", "query"]
        .into_iter()
        .find_map(|key| fields.get(key)?.as_str().map(ToOwned::to_owned))
}

fn parse_claude_result(event: ClaudeEvent) -> Result<RunLogEvent, RunLogParseError> {
    let mut result = if event.subtype == "success" && !event.is_error {
        RunResult::succeeded()
    } else {
        let message = if event.result.is_empty() {
            event.subtype
        } else {
            event.result
        };
        RunResult::failed(message)
    };
    if let Some(duration_ms) = event.duration_ms {
        result = result.with_duration(Duration::from_millis(duration_ms));
    }
    if let Some(turns) = event.num_turns {
        result = result.with_turns(turns);
    }
    if let Some(cost) = event.total_cost_usd {
        result = result.with_cost(parse_claude_cost(&cost)?);
    }
    Ok(RunLogEvent::Result(result))
}

fn parse_claude_cost(cost: &Number) -> Result<RunCost, RunLogParseError> {
    let value = cost
        .as_f64()
        .filter(|value| value.is_finite() && *value >= 0.0)
        .ok_or_else(|| RunLogParseError::InvalidCost {
            runtime: AgentRuntime::Claude,
            value: cost.to_string(),
        })?;
    let micro_usd = (value * 1_000_000.0).round();
    if micro_usd > u64::MAX as f64 {
        return Err(RunLogParseError::InvalidCost {
            runtime: AgentRuntime::Claude,
            value: cost.to_string(),
        });
    }
    Ok(RunCost::from_micro_usd(micro_usd as u64))
}

/// Token usage normalized across Agent Runtime providers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunUsage {
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
}

impl RunUsage {
    /// Creates usage with required input and output token counts.
    pub const fn new(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens,
            cached_input_tokens: 0,
            cache_write_input_tokens: 0,
            output_tokens,
            reasoning_output_tokens: 0,
        }
    }

    /// Records input tokens read from a provider cache.
    pub const fn with_cached_input_tokens(mut self, tokens: u64) -> Self {
        self.cached_input_tokens = tokens;
        self
    }

    /// Records input tokens written to a provider cache.
    pub const fn with_cache_write_input_tokens(mut self, tokens: u64) -> Self {
        self.cache_write_input_tokens = tokens;
        self
    }

    /// Records output tokens used for provider reasoning.
    pub const fn with_reasoning_output_tokens(mut self, tokens: u64) -> Self {
        self.reasoning_output_tokens = tokens;
        self
    }

    /// Returns provider-reported input tokens.
    pub const fn input_tokens(&self) -> u64 {
        self.input_tokens
    }

    /// Returns input tokens read from a provider cache.
    pub const fn cached_input_tokens(&self) -> u64 {
        self.cached_input_tokens
    }

    /// Returns input tokens written to a provider cache.
    pub const fn cache_write_input_tokens(&self) -> u64 {
        self.cache_write_input_tokens
    }

    /// Returns output tokens.
    pub const fn output_tokens(&self) -> u64 {
        self.output_tokens
    }

    /// Returns output tokens used for provider reasoning.
    pub const fn reasoning_output_tokens(&self) -> u64 {
        self.reasoning_output_tokens
    }
}

/// Progress status shared by provider-neutral Run activities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunActivityStatus {
    /// The activity is still executing.
    InProgress,
    /// The activity completed successfully.
    Completed,
    /// The activity failed.
    Failed {
        /// Provider-reported failure detail, when available.
        message: Option<String>,
    },
    /// The activity was declined before execution.
    Declined,
}

/// One participant reported by an Agent Runtime collaboration event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollaborationParticipant {
    id: String,
    status: String,
    message: Option<String>,
}

impl CollaborationParticipant {
    /// Creates a participant with its provider-reported status.
    pub fn new(id: impl Into<String>, status: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            status: status.into(),
            message: None,
        }
    }

    /// Adds provider-reported participant detail.
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// Returns the provider's participant identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the open-ended provider status.
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Returns provider-reported participant detail, when available.
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }
}

/// Structured activity for one Agent Runtime collaboration operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollaborationEvent {
    operation: String,
    status: RunActivityStatus,
    sender: Option<String>,
    receivers: Vec<String>,
    prompt: Option<String>,
    participants: Vec<CollaborationParticipant>,
}

impl CollaborationEvent {
    /// Creates a collaboration event for an open-ended provider operation.
    pub fn new(operation: impl Into<String>, status: RunActivityStatus) -> Self {
        Self {
            operation: operation.into(),
            status,
            sender: None,
            receivers: Vec::new(),
            prompt: None,
            participants: Vec::new(),
        }
    }

    /// Adds the provider's sending Session or participant identifier.
    pub fn with_sender(mut self, sender: impl Into<String>) -> Self {
        self.sender = Some(sender.into());
        self
    }

    /// Adds receiving Session or participant identifiers.
    pub fn with_receivers<I, S>(mut self, receivers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.receivers = receivers.into_iter().map(Into::into).collect();
        self
    }

    /// Adds the delegated prompt when the provider reports it.
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = Some(prompt.into());
        self
    }

    /// Adds one provider-reported participant state.
    pub fn with_participant(mut self, participant: CollaborationParticipant) -> Self {
        self.participants.push(participant);
        self
    }

    /// Returns the open-ended provider operation.
    pub fn operation(&self) -> &str {
        &self.operation
    }

    /// Returns the normalized operation status.
    pub const fn status(&self) -> &RunActivityStatus {
        &self.status
    }

    /// Returns the sending identifier, when available.
    pub fn sender(&self) -> Option<&str> {
        self.sender.as_deref()
    }

    /// Returns receiving identifiers in provider order.
    pub fn receivers(&self) -> &[String] {
        &self.receivers
    }

    /// Returns the delegated prompt, when available.
    pub fn prompt(&self) -> Option<&str> {
        self.prompt.as_deref()
    }

    /// Returns provider-reported participant states.
    pub fn participants(&self) -> &[CollaborationParticipant] {
        &self.participants
    }
}

/// One meaningful activity observed in an Agent Runtime log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunActivityKind {
    /// The Agent Runtime started an Agent Session.
    SessionStarted,
    /// The Agent Runtime emitted a conversational message.
    Message {
        /// Message text.
        text: String,
    },
    /// The Agent Runtime invoked or completed a tool.
    Tool {
        /// Provider-neutral tool name.
        name: String,
        /// Concise tool input or target, when available.
        detail: Option<String>,
        /// Current tool status.
        status: RunActivityStatus,
    },
    /// The Agent Runtime changed files.
    FileChanges {
        /// Changed paths exactly as reported by the provider.
        paths: Vec<String>,
    },
    /// The Agent Runtime updated its plan.
    Plan {
        /// Number of completed plan items.
        completed: usize,
        /// Total number of plan items.
        total: usize,
    },
    /// The Agent Runtime reported reasoning text.
    Reasoning {
        /// Reasoning text.
        text: String,
    },
    /// The Agent Runtime reported a non-terminal warning.
    Warning {
        /// Warning detail.
        message: String,
    },
    /// The Agent Runtime reported collaboration with another Agent Session.
    Collaboration(CollaborationEvent),
}

/// One provider-neutral Run activity with optional token usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunActivity {
    kind: RunActivityKind,
    usage: Option<RunUsage>,
}

impl RunActivity {
    /// Creates an activity without token usage.
    pub const fn new(kind: RunActivityKind) -> Self {
        Self { kind, usage: None }
    }

    /// Adds the usage observed with this activity.
    pub fn with_usage(mut self, usage: RunUsage) -> Self {
        self.usage = Some(usage);
        self
    }

    /// Returns the activity kind.
    pub const fn kind(&self) -> &RunActivityKind {
        &self.kind
    }

    /// Returns usage observed with this activity, when available.
    pub const fn usage(&self) -> Option<&RunUsage> {
        self.usage.as_ref()
    }
}

/// Exact Run cost in millionths of one US dollar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RunCost(u64);

impl RunCost {
    /// Creates a Run cost from millionths of one US dollar.
    pub const fn from_micro_usd(micro_usd: u64) -> Self {
        Self(micro_usd)
    }

    /// Returns the cost in millionths of one US dollar.
    pub const fn as_micro_usd(self) -> u64 {
        self.0
    }
}

/// The provider-reported conclusion of a Run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunConclusion {
    /// The Agent Runtime reported successful completion.
    Succeeded,
    /// The Agent Runtime reported a failure.
    Failed {
        /// Provider-neutral failure message.
        message: String,
    },
}

/// Structured completion details reported by an Agent Runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunResult {
    conclusion: RunConclusion,
    usage: Option<RunUsage>,
    duration: Option<Duration>,
    turns: Option<u64>,
    cost: Option<RunCost>,
}

impl RunResult {
    /// Creates a successful Run result.
    pub const fn succeeded() -> Self {
        Self {
            conclusion: RunConclusion::Succeeded,
            usage: None,
            duration: None,
            turns: None,
            cost: None,
        }
    }

    /// Creates a failed Run result.
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            conclusion: RunConclusion::Failed {
                message: message.into(),
            },
            usage: None,
            duration: None,
            turns: None,
            cost: None,
        }
    }

    /// Adds normalized token usage.
    pub fn with_usage(mut self, usage: RunUsage) -> Self {
        self.usage = Some(usage);
        self
    }

    /// Adds provider-reported Run duration.
    pub const fn with_duration(mut self, duration: Duration) -> Self {
        self.duration = Some(duration);
        self
    }

    /// Adds the number of provider-reported turns.
    pub const fn with_turns(mut self, turns: u64) -> Self {
        self.turns = Some(turns);
        self
    }

    /// Adds the provider-reported Run cost.
    pub const fn with_cost(mut self, cost: RunCost) -> Self {
        self.cost = Some(cost);
        self
    }

    /// Returns the provider-reported conclusion.
    pub const fn conclusion(&self) -> &RunConclusion {
        &self.conclusion
    }

    /// Returns normalized token usage when the provider reported it.
    pub const fn usage(&self) -> Option<&RunUsage> {
        self.usage.as_ref()
    }

    /// Returns provider-reported duration when available.
    pub const fn duration(&self) -> Option<Duration> {
        self.duration
    }

    /// Returns the number of provider-reported turns when available.
    pub const fn turns(&self) -> Option<u64> {
        self.turns
    }

    /// Returns provider-reported cost when available.
    pub const fn cost(&self) -> Option<RunCost> {
        self.cost
    }
}
