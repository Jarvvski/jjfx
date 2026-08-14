//! Linear Ticket discovery and provider-neutral Dispatch inputs.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use thiserror::Error;

use crate::{AgentRuntime, TicketId};

const DEFAULT_PI_HELPER_TIMEOUT: Duration = Duration::from_secs(30);
const PI_HELPER_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Configuration for the dedicated read-only Pi discovery helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiDiscoveryHelper {
    executable: PathBuf,
    timeout: Duration,
}

impl PiDiscoveryHelper {
    /// Selects the helper executable with the production timeout.
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            timeout: DEFAULT_PI_HELPER_TIMEOUT,
        }
    }

    /// Overrides the bounded execution time.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn run(&self, workspace: &std::path::Path, input: &str) -> Result<Vec<u8>, TicketQueryError> {
        let mut child = Command::new(&self.executable)
            .current_dir(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| {
                TicketQueryError::setup(format!(
                    "cannot start pi ticket discovery helper: {source}"
                ))
            })?;
        let stdout = child
            .stdout
            .take()
            .expect("stdout is piped for every Pi discovery helper");
        let stderr = child
            .stderr
            .take()
            .expect("stderr is piped for every Pi discovery helper");
        let stdout_reader = thread::spawn(move || read_pipe(stdout));
        let stderr_reader = thread::spawn(move || read_pipe(stderr));
        let write_result = child
            .stdin
            .take()
            .expect("stdin is piped for every Pi discovery helper")
            .write_all(input.as_bytes());
        if let Err(source) = write_result {
            let _ = child.kill();
            let _ = child.wait();
            let _ = finish_pipe(stdout_reader, "stdout");
            let _ = finish_pipe(stderr_reader, "stderr");
            return Err(TicketQueryError::transport(
                format!("cannot write pi helper request: {source}"),
                false,
            ));
        }

        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = finish_pipe(stdout_reader, "stdout");
                    let _ = finish_pipe(stderr_reader, "stderr");
                    return Err(TicketQueryError::timeout(
                        "pi ticket discovery helper exceeded its 30-second execution limit",
                    ));
                }
                Ok(None) => thread::sleep(PI_HELPER_POLL_INTERVAL),
                Err(source) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = finish_pipe(stdout_reader, "stdout");
                    let _ = finish_pipe(stderr_reader, "stderr");
                    return Err(TicketQueryError::transport(
                        format!("cannot wait for pi helper: {source}"),
                        true,
                    ));
                }
            }
        };
        let stdout = finish_pipe(stdout_reader, "stdout")?;
        let _stderr = finish_pipe(stderr_reader, "stderr")?;
        successful_helper_output(status, stdout)
    }
}

fn read_pipe(mut pipe: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn finish_pipe(
    reader: thread::JoinHandle<io::Result<Vec<u8>>>,
    name: &'static str,
) -> Result<Vec<u8>, TicketQueryError> {
    reader
        .join()
        .map_err(|_| {
            TicketQueryError::transport(format!("pi helper {name} reader panicked"), false)
        })?
        .map_err(|source| {
            TicketQueryError::transport(format!("cannot read pi helper {name}: {source}"), true)
        })
}

fn successful_helper_output(
    status: ExitStatus,
    stdout: Vec<u8>,
) -> Result<Vec<u8>, TicketQueryError> {
    if status.success() {
        Ok(stdout)
    } else {
        Err(TicketQueryError::transport(
            format!("pi ticket discovery helper exited with {status}"),
            true,
        ))
    }
}

/// A short-lived Agent Runtime adapter for read-only Linear discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRuntimeQuery {
    runtime: AgentRuntime,
    workspace: PathBuf,
    pi_helper: Option<PiDiscoveryHelper>,
}

impl AgentRuntimeQuery {
    /// Selects the Agent Runtime and working directory for short-lived queries.
    pub fn new(runtime: AgentRuntime, workspace: impl Into<PathBuf>) -> Self {
        Self {
            runtime,
            workspace: workspace.into(),
            pi_helper: None,
        }
    }

    /// Configures the dedicated read-only helper used for Pi discovery.
    pub fn with_pi_helper(mut self, helper: PiDiscoveryHelper) -> Self {
        self.pi_helper = Some(helper);
        self
    }

    fn command(&self, request: &TicketQueryRequest) -> Command {
        let prompt = request.prompt();
        let mut command = Command::new(self.runtime.as_str());
        command.current_dir(&self.workspace);
        match self.runtime {
            AgentRuntime::Claude => {
                command.args([
                    "-p",
                    "--output-format",
                    "json",
                    "--no-session-persistence",
                    "--allowedTools=mcp__claude_ai_Linear__list_issues,mcp__claude_ai_Linear__get_issue",
                    &prompt,
                ]);
            }
            AgentRuntime::Codex => {
                command.args([
                    "--sandbox",
                    "read-only",
                    "--ask-for-approval",
                    "never",
                    "exec",
                    "--ephemeral",
                    "--skip-git-repo-check",
                    &prompt,
                ]);
            }
            AgentRuntime::Pi => {
                command.arg(prompt);
            }
        }
        command
    }

    fn pi_query(&self, request: &TicketQueryRequest) -> Result<String, TicketQueryError> {
        let helper = self.pi_helper.as_ref().ok_or_else(|| {
            TicketQueryError::setup(
                "pi ticket discovery requires the JJFX_PI_LINEAR_HELPER configuration",
            )
        })?;
        let input = request.pi_helper_input()?;
        let output = helper.run(&self.workspace, &input)?;
        let response = serde_json::from_slice::<PiHelperResponse>(&output).map_err(|error| {
            TicketQueryError::protocol(format!(
                "pi ticket discovery helper returned a malformed response envelope: {error}"
            ))
        })?;
        if response.version != PI_HELPER_PROTOCOL_VERSION {
            return Err(TicketQueryError::protocol(format!(
                "pi ticket discovery helper returned unsupported protocol version {}",
                response.version
            )));
        }
        if let Some(error) = response.error {
            let message = format!(
                "pi ticket discovery helper reported {}: {}",
                error.kind, error.message
            );
            return Err(match error.kind.as_str() {
                "authentication" => TicketQueryError::authentication(message),
                "unsupported" | "not_configured" => TicketQueryError::unsupported(message),
                "permanent" => TicketQueryError::permanent(message),
                _ => TicketQueryError::protocol(format!(
                    "pi ticket discovery helper returned unknown error kind {:?}",
                    error.kind
                )),
            });
        }
        response
            .result
            .ok_or_else(|| {
                TicketQueryError::protocol(
                    "pi ticket discovery helper response contains no result or error",
                )
            })
            .and_then(|result| {
                serde_json::to_string(&result).map_err(|error| {
                    TicketQueryError::permanent(format!(
                        "cannot normalize pi helper result: {error}"
                    ))
                })
            })
    }
}

impl TicketQuery for AgentRuntimeQuery {
    fn query(&self, request: &TicketQueryRequest) -> Result<String, TicketQueryError> {
        if self.runtime == AgentRuntime::Pi {
            return self.pi_query(request);
        }
        let output = self.command(request).output().map_err(|source| {
            TicketQueryError::permanent(format!("cannot start {} query: {source}", self.runtime))
        })?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if !output.status.success() {
            let diagnostic = if stderr.is_empty() { stdout } else { stderr };
            return Err(TicketQueryError::transient(format!(
                "{} query failed: {diagnostic}",
                self.runtime
            )));
        }
        normalize_query_output(self.runtime, &stdout).ok_or_else(|| {
            TicketQueryError::transient(format!("{} query returned no JSON object", self.runtime))
        })
    }
}

fn normalize_query_output(runtime: AgentRuntime, output: &str) -> Option<String> {
    let output = match runtime {
        AgentRuntime::Claude => {
            #[derive(Deserialize)]
            struct ClaudeResult {
                result: String,
            }

            serde_json::from_str::<ClaudeResult>(output)
                .ok()
                .filter(|wrapper| !wrapper.result.is_empty())
                .map_or_else(|| output.to_owned(), |wrapper| wrapper.result)
        }
        AgentRuntime::Codex => codex_query_text(output).unwrap_or_else(|| output.to_owned()),
        AgentRuntime::Pi => output.to_owned(),
    };
    extract_json_object(&output).map(str::to_owned)
}

fn codex_query_text(output: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct CodexQueryEvent {
        item: Option<CodexQueryItem>,
    }

    #[derive(Deserialize)]
    struct CodexQueryItem {
        #[serde(rename = "type")]
        item_type: String,
        #[serde(default)]
        text: String,
    }

    output
        .lines()
        .filter_map(|line| serde_json::from_str::<CodexQueryEvent>(line).ok())
        .filter_map(|event| event.item)
        .filter(|item| item.item_type == "agent_message" && !item.text.trim().is_empty())
        .map(|item| item.text)
        .next_back()
}

fn extract_json_object(output: &str) -> Option<&str> {
    let start = output.find('{')?;
    let mut depth = 0_u32;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, character) in output[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&output[start..start + offset + character.len_utf8()]);
                }
            }
            _ => {}
        }
    }
    None
}

/// A provider-neutral request for read-only Ticket discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TicketQueryRequest {
    /// Finds Tickets matching the configured dispatch filter.
    ReadyTickets {
        /// The validated label and workflow status to match.
        filter: ReadyTicketFilter,
    },
    /// Finds a parent Ticket's direct children and their dependencies.
    DependencyGraph {
        /// The parent whose direct children should be returned.
        parent: ParentTicket,
        /// The repository used to identify cross-repository children.
        repository: RepositoryIdentity,
    },
}

const PI_HELPER_PROTOCOL_VERSION: u8 = 1;

#[derive(Debug, Deserialize)]
struct PiHelperResponse {
    version: u8,
    result: Option<serde_json::Value>,
    error: Option<PiHelperError>,
}

#[derive(Debug, Deserialize)]
struct PiHelperError {
    kind: String,
    message: String,
}

impl TicketQueryRequest {
    fn prompt(&self) -> String {
        match self {
            Self::ReadyTickets { filter } => format!(
                "Use Linear to find issues with label {:?} in {:?} status. Return only JSON as {{\"tickets\":[{{\"id\":\"AMBA-42\",\"title\":\"Title\",\"status\":\"{}\",\"labels\":[{:?}]}}]}}.",
                filter.label,
                filter.status.as_str(),
                filter.status.as_str(),
                filter.label,
            ),
            Self::DependencyGraph { parent, repository } => format!(
                "Fetch the direct children of Linear issue {} and their blockedBy relations. Determine cross_repo relative to {}. Return only JSON as {{\"sub_issues\":[{{\"id\":\"AMBA-42\",\"title\":\"Title\",\"status\":\"Todo\",\"blocked_by\":[\"AMBA-41\"],\"cross_repo\":false}}]}}.",
                parent.id(),
                repository.as_str(),
            ),
        }
    }

    fn pi_helper_input(&self) -> Result<String, TicketQueryError> {
        let input = match self {
            Self::ReadyTickets { filter } => serde_json::json!({
                "version": PI_HELPER_PROTOCOL_VERSION,
                "operation": "ready_tickets",
                "label": filter.label,
                "status": filter.status.as_str(),
            }),
            Self::DependencyGraph { parent, repository } => serde_json::json!({
                "version": PI_HELPER_PROTOCOL_VERSION,
                "operation": "dependency_graph",
                "parent": parent.id().as_str(),
                "repository": repository.as_str(),
            }),
        };
        serde_json::to_string(&input).map_err(|error| {
            TicketQueryError::permanent(format!("cannot encode pi helper request: {error}"))
        })
    }
}

/// Executes one short-lived, read-only query against Linear through an Agent Runtime.
pub trait TicketQuery {
    /// Returns the provider-neutral text response for a typed request.
    fn query(&self, request: &TicketQueryRequest) -> Result<String, TicketQueryError>;
}

/// Stable classifications for Ticket query adapter failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketQueryErrorKind {
    /// Required executable or configuration is absent or invalid.
    Setup,
    /// The helper could not be started or completed normally.
    Transport,
    /// The helper exceeded its bounded execution time.
    Timeout,
    /// The configured transport rejected its credentials.
    Authentication,
    /// The configured transport does not provide the requested capability.
    Unsupported,
    /// The helper violated the versioned request/response contract.
    Protocol,
    /// A provider query failed without a more specific classification.
    Query,
}

/// A failure reported by a Ticket query adapter.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct TicketQueryError {
    kind: TicketQueryErrorKind,
    message: String,
    retryable: bool,
}

impl TicketQueryError {
    /// Creates a failure that may recover on the single bounded retry.
    pub fn transient(message: impl Into<String>) -> Self {
        Self::new(TicketQueryErrorKind::Query, message, true)
    }

    /// Creates a failure that must surface without repeating the query.
    pub fn permanent(message: impl Into<String>) -> Self {
        Self::new(TicketQueryErrorKind::Query, message, false)
    }

    fn setup(message: impl Into<String>) -> Self {
        Self::new(TicketQueryErrorKind::Setup, message, false)
    }

    fn transport(message: impl Into<String>, retryable: bool) -> Self {
        Self::new(TicketQueryErrorKind::Transport, message, retryable)
    }

    fn timeout(message: impl Into<String>) -> Self {
        Self::new(TicketQueryErrorKind::Timeout, message, true)
    }

    fn authentication(message: impl Into<String>) -> Self {
        Self::new(TicketQueryErrorKind::Authentication, message, false)
    }

    fn unsupported(message: impl Into<String>) -> Self {
        Self::new(TicketQueryErrorKind::Unsupported, message, false)
    }

    fn protocol(message: impl Into<String>) -> Self {
        Self::new(TicketQueryErrorKind::Protocol, message, false)
    }

    fn new(
        kind: TicketQueryErrorKind,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            retryable,
        }
    }

    /// Returns the stable failure classification.
    pub fn kind(&self) -> TicketQueryErrorKind {
        self.kind
    }

    /// Reports whether Ticket discovery may repeat this query once.
    pub fn is_retryable(&self) -> bool {
        self.retryable
    }
}

/// Selects Ready Tickets by dispatch label and expected Linear workflow status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyTicketFilter {
    label: String,
    status: TicketStatus,
}

impl ReadyTicketFilter {
    /// Creates a validated Ready Ticket filter.
    pub fn new(label: impl Into<String>, status: TicketStatus) -> Result<Self, TicketValueError> {
        let label = label.into().trim().to_owned();
        if label.is_empty() {
            return Err(TicketValueError::BlankLabel);
        }
        Ok(Self { label, status })
    }
}

/// A canonical GitHub Repository identity used to scope Dispatch delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryIdentity(String);

impl RepositoryIdentity {
    /// Validates an `owner/name` Repository slug.
    pub fn parse(value: impl Into<String>) -> Result<Self, TicketValueError> {
        let value = value.into().trim().to_owned();
        let mut parts = value.split('/');
        if parts.next().is_none_or(str::is_empty)
            || parts.next().is_none_or(str::is_empty)
            || parts.next().is_some()
        {
            return Err(TicketValueError::InvalidRepositoryIdentity(value));
        }
        Ok(Self(value))
    }

    /// Returns the canonical `owner/name` slug.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A safely excluded discovery entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryDiagnostic {
    subject: String,
    reason: String,
}

impl DiscoveryDiagnostic {
    fn new(subject: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            reason: reason.into(),
        }
    }

    /// Returns the provider-supplied identifier associated with the entry.
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// Returns why the entry was excluded or repaired.
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// Ready Tickets plus diagnostics for entries that were safely excluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyTickets {
    tickets: Vec<Ticket>,
    diagnostics: Vec<DiscoveryDiagnostic>,
}

impl ReadyTickets {
    /// Returns validated Ready Tickets in provider order.
    pub fn tickets(&self) -> &[Ticket] {
        &self.tickets
    }

    /// Returns diagnostics for excluded provider entries.
    pub fn diagnostics(&self) -> &[DiscoveryDiagnostic] {
        &self.diagnostics
    }
}

/// Discovers typed Linear Tickets behind one provider-neutral interface.
#[derive(Debug)]
pub struct TicketDiscovery<Q> {
    query: Q,
}

impl<Q> TicketDiscovery<Q>
where
    Q: TicketQuery,
{
    /// Creates discovery over a production or deterministic query adapter.
    pub fn new(query: Q) -> Self {
        Self { query }
    }

    /// Discovers Tickets carrying the configured label in the expected status.
    pub fn ready_tickets(
        &self,
        filter: &ReadyTicketFilter,
    ) -> Result<ReadyTickets, TicketDiscoveryError> {
        let request = TicketQueryRequest::ReadyTickets {
            filter: filter.clone(),
        };
        let payload: RawReadyTickets = self.query_payload(&request)?;
        let mut tickets = Vec::with_capacity(payload.tickets.len());
        let mut diagnostics = Vec::new();
        for raw in payload.tickets {
            match ready_ticket(raw, &filter.label, &filter.status) {
                Ok(ticket) => tickets.push(ticket),
                Err(diagnostic) => diagnostics.push(diagnostic),
            }
        }
        Ok(ReadyTickets {
            tickets,
            diagnostics,
        })
    }

    /// Discovers and validates the direct children of one Parent Ticket.
    pub fn dependency_graph(
        &self,
        parent: &ParentTicket,
        repository: &RepositoryIdentity,
    ) -> Result<DependencyGraph, TicketDiscoveryError> {
        let request = TicketQueryRequest::DependencyGraph {
            parent: parent.clone(),
            repository: repository.clone(),
        };
        let payload: RawDependencyGraph = self.query_payload(&request)?;
        let entry_count = payload.sub_issues.len();
        let mut child_counts = BTreeMap::new();
        for raw in &payload.sub_issues {
            *child_counts.entry(raw.id.clone()).or_insert(0_usize) += 1;
        }
        let duplicate_children = child_counts
            .into_iter()
            .filter_map(|(id, count)| (count > 1).then_some(id))
            .collect::<BTreeSet<_>>();
        let mut reported_duplicates = BTreeSet::new();
        let mut candidates = BTreeMap::new();
        let mut diagnostics = Vec::new();
        for raw in payload.sub_issues {
            let subject = raw.id.clone();
            if duplicate_children.contains(&subject) {
                if reported_duplicates.insert(subject.clone()) {
                    diagnostics.push(DiscoveryDiagnostic::new(&subject, "duplicate child"));
                }
                continue;
            }
            let id = match TicketId::parse(raw.id) {
                Ok(id) => id,
                Err(error) => {
                    diagnostics.push(DiscoveryDiagnostic::new(subject, error.to_string()));
                    continue;
                }
            };
            if &id == parent.id() {
                diagnostics.push(DiscoveryDiagnostic::new(
                    id.as_str(),
                    "parent cannot be its own child",
                ));
                continue;
            }
            let title = match TicketTitle::parse(raw.title) {
                Ok(title) => title,
                Err(error) => {
                    diagnostics.push(DiscoveryDiagnostic::new(id.as_str(), error.to_string()));
                    continue;
                }
            };
            let status = match TicketStatus::parse(raw.status) {
                Ok(status) => status,
                Err(error) => {
                    diagnostics.push(DiscoveryDiagnostic::new(id.as_str(), error.to_string()));
                    continue;
                }
            };
            candidates.insert(
                id.clone(),
                (
                    Ticket::new(id, title, status),
                    raw.blocked_by,
                    raw.cross_repo,
                ),
            );
        }

        loop {
            let known_children = candidates.keys().cloned().collect::<BTreeSet<_>>();
            let mut unsafe_children = BTreeSet::new();
            for (id, (_, raw_blockers, _)) in &candidates {
                for raw_blocker in raw_blockers {
                    let blocker = match TicketId::parse(raw_blocker.clone()) {
                        Ok(blocker) => blocker,
                        Err(error) => {
                            diagnostics.push(DiscoveryDiagnostic::new(
                                id.as_str(),
                                format!("invalid Blocker: {error}"),
                            ));
                            unsafe_children.insert(id.clone());
                            continue;
                        }
                    };
                    if blocker == *id {
                        diagnostics.push(DiscoveryDiagnostic::new(id.as_str(), "self-blocker"));
                        unsafe_children.insert(id.clone());
                    } else if !known_children.contains(&blocker) {
                        diagnostics.push(DiscoveryDiagnostic::new(
                            id.as_str(),
                            format!("unknown Blocker {blocker}"),
                        ));
                        unsafe_children.insert(id.clone());
                    }
                }
            }
            if unsafe_children.is_empty() {
                break;
            }
            for id in unsafe_children {
                candidates.remove(&id);
            }
        }

        if entry_count > 0 && candidates.is_empty() {
            return Err(TicketDiscoveryError::UnusableGraph {
                invalid_entries: diagnostics.len(),
            });
        }

        let mut sub_issues = BTreeMap::new();
        for (id, (ticket, raw_blockers, cross_repository)) in candidates {
            let blockers = raw_blockers
                .into_iter()
                .map(TicketId::parse)
                .collect::<Result<BTreeSet<_>, _>>()
                .map_err(|_| TicketDiscoveryError::UnusableGraph {
                    invalid_entries: diagnostics.len() + 1,
                })?
                .into_iter()
                .map(Blocker::new)
                .collect();
            sub_issues.insert(
                id,
                DiscoveredSubIssue::new(ticket, blockers, cross_repository),
            );
        }
        Ok(DependencyGraph {
            parent: parent.clone(),
            sub_issues,
            diagnostics,
        })
    }

    fn query_payload<T>(&self, request: &TicketQueryRequest) -> Result<T, TicketDiscoveryError>
    where
        T: DeserializeOwned,
    {
        let first = match self.query_attempt(request) {
            Ok(payload) => return Ok(payload),
            Err(error) => error,
        };
        if !first.retryable() {
            return Err(TicketDiscoveryError::QueryFailed {
                error: first.to_string(),
            });
        }
        self.query_attempt(request)
            .map_err(|second| TicketDiscoveryError::RetriesExhausted {
                first: first.to_string(),
                second: second.to_string(),
            })
    }

    fn query_attempt<T>(&self, request: &TicketQueryRequest) -> Result<T, QueryAttemptError>
    where
        T: DeserializeOwned,
    {
        let output = self.query.query(request)?;
        serde_json::from_str(&output).map_err(QueryAttemptError::MalformedResponse)
    }
}

#[derive(Debug, Error)]
enum QueryAttemptError {
    #[error("query failed: {0}")]
    Query(#[from] TicketQueryError),
    #[error("response was malformed: {0}")]
    MalformedResponse(serde_json::Error),
}

impl QueryAttemptError {
    fn retryable(&self) -> bool {
        match self {
            Self::Query(error) => error.retryable,
            Self::MalformedResponse(_) => true,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawReadyTickets {
    tickets: Vec<RawTicket>,
}

#[derive(Debug, Deserialize)]
struct RawTicket {
    id: String,
    title: String,
    status: String,
    labels: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawDependencyGraph {
    sub_issues: Vec<RawSubIssue>,
}

#[derive(Debug, Deserialize)]
struct RawSubIssue {
    id: String,
    title: String,
    status: String,
    blocked_by: Vec<String>,
    cross_repo: bool,
}

fn ready_ticket(
    raw: RawTicket,
    expected_label: &str,
    expected_status: &TicketStatus,
) -> Result<Ticket, DiscoveryDiagnostic> {
    let subject = raw.id.clone();
    let id = TicketId::parse(raw.id)
        .map_err(|error| DiscoveryDiagnostic::new(&subject, error.to_string()))?;
    let title = TicketTitle::parse(raw.title)
        .map_err(|error| DiscoveryDiagnostic::new(&subject, error.to_string()))?;
    let status = TicketStatus::parse(raw.status)
        .map_err(|error| DiscoveryDiagnostic::new(&subject, error.to_string()))?;
    if !raw.labels.iter().any(|label| label == expected_label) {
        return Err(DiscoveryDiagnostic::new(
            subject,
            format!("missing label {expected_label:?}"),
        ));
    }
    if status != *expected_status {
        return Err(DiscoveryDiagnostic::new(
            subject,
            format!(
                "expected status {:?}, found {:?}",
                expected_status.as_str(),
                status.as_str()
            ),
        ));
    }
    Ok(Ticket::new(id, title, status))
}

/// A failure that makes a Ticket discovery result unusable as a whole.
#[derive(Debug, Error)]
pub enum TicketDiscoveryError {
    /// A permanent query failure surfaced without a retry.
    #[error("Ticket discovery query failed: {error}")]
    QueryFailed {
        /// Adapter context for the permanent failure.
        error: String,
    },
    /// Both bounded query attempts failed.
    #[error(
        "Ticket discovery failed after one retry: first attempt {first}; second attempt {second}"
    )]
    RetriesExhausted {
        /// Context from the initial failure.
        first: String,
        /// Context from the retry failure.
        second: String,
    },
    /// Every returned child was invalid, so an empty result would be unsafe.
    #[error(
        "Ticket query returned an unusable dependency graph ({invalid_entries} invalid entries)"
    )]
    UnusableGraph {
        /// Number of diagnostics produced while validating the graph.
        invalid_entries: usize,
    },
}

/// A non-empty human-facing Ticket title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketTitle(String);

impl TicketTitle {
    /// Validates and normalizes a title returned by Linear discovery.
    pub fn parse(value: impl Into<String>) -> Result<Self, TicketValueError> {
        let value = value.into().trim().to_owned();
        if value.is_empty() {
            return Err(TicketValueError::BlankTitle);
        }
        Ok(Self(value))
    }

    /// Returns the normalized title.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A non-empty, forward-compatible Linear workflow status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketStatus(String);

impl TicketStatus {
    /// Validates and normalizes a status returned by Linear discovery.
    pub fn parse(value: impl Into<String>) -> Result<Self, TicketValueError> {
        let value = value.into().trim().to_owned();
        if value.is_empty() {
            return Err(TicketValueError::BlankStatus);
        }
        Ok(Self(value))
    }

    /// Returns the normalized workflow status.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A typed Linear work item selected for Workspace Dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    id: TicketId,
    title: TicketTitle,
    status: TicketStatus,
}

impl Ticket {
    /// Creates a Ticket from validated discovery values.
    pub fn new(id: TicketId, title: TicketTitle, status: TicketStatus) -> Self {
        Self { id, title, status }
    }

    /// Returns the stable Linear identifier.
    pub fn id(&self) -> &TicketId {
        &self.id
    }

    /// Returns the human-facing title.
    pub fn title(&self) -> &TicketTitle {
        &self.title
    }

    /// Returns the Linear workflow status.
    pub fn status(&self) -> &TicketStatus {
        &self.status
    }
}

/// A Ticket whose direct children may form a Dispatch Group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentTicket(TicketId);

impl ParentTicket {
    /// Marks a Ticket identifier as the parent being discovered.
    pub fn new(id: TicketId) -> Self {
        Self(id)
    }

    /// Returns the Parent Ticket identifier.
    pub fn id(&self) -> &TicketId {
        &self.0
    }
}

/// A sibling Sub-issue that must complete before another Sub-issue.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Blocker(TicketId);

impl Blocker {
    /// Marks a Ticket identifier as a Blocker.
    pub fn new(id: TicketId) -> Self {
        Self(id)
    }

    /// Returns the Blocker's Ticket identifier.
    pub fn id(&self) -> &TicketId {
        &self.0
    }
}

/// A validated direct child and its sibling Blockers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredSubIssue {
    ticket: Ticket,
    blockers: Vec<Blocker>,
    cross_repository: bool,
}

impl DiscoveredSubIssue {
    fn new(ticket: Ticket, blockers: Vec<Blocker>, cross_repository: bool) -> Self {
        Self {
            ticket,
            blockers,
            cross_repository,
        }
    }

    /// Returns the typed child Ticket.
    pub fn ticket(&self) -> &Ticket {
        &self.ticket
    }

    /// Returns direct sibling Blockers.
    pub fn blockers(&self) -> &[Blocker] {
        &self.blockers
    }

    /// Returns whether the child targets another Repository.
    pub fn is_cross_repository(&self) -> bool {
        self.cross_repository
    }
}

/// A Parent Ticket's validated direct children and Dependencies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyGraph {
    parent: ParentTicket,
    sub_issues: BTreeMap<TicketId, DiscoveredSubIssue>,
    diagnostics: Vec<DiscoveryDiagnostic>,
}

impl DependencyGraph {
    /// Returns the Parent Ticket whose children were discovered.
    pub fn parent(&self) -> &ParentTicket {
        &self.parent
    }

    /// Returns all direct children keyed by Ticket identifier.
    pub fn sub_issues(&self) -> &BTreeMap<TicketId, DiscoveredSubIssue> {
        &self.sub_issues
    }

    /// Returns one direct child by Ticket identifier.
    pub fn sub_issue(&self, id: &TicketId) -> Option<&DiscoveredSubIssue> {
        self.sub_issues.get(id)
    }

    /// Returns diagnostics for safely excluded or repaired relationships.
    pub fn diagnostics(&self) -> &[DiscoveryDiagnostic] {
        &self.diagnostics
    }
}

/// Invalid provider-neutral Ticket data.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TicketValueError {
    /// Repository identity was not an `owner/name` slug.
    #[error("Repository identity {0:?} must use owner/name format")]
    InvalidRepositoryIdentity(String),
    /// Ready Ticket discovery was configured without a label.
    #[error("Ready Ticket label cannot be blank")]
    BlankLabel,
    /// Linear returned no usable title.
    #[error("Ticket title cannot be blank")]
    BlankTitle,
    /// Linear returned no usable workflow status.
    #[error("Ticket status cannot be blank")]
    BlankStatus,
}
