//! A typed, best-effort client for opencode's server API.
//!
//! opencode exposes an HTTP API over its sessions, and jjfx reaches it the way
//! it already reaches `jj` and `gh`: by shelling out, here to
//! `opencode api <operationId> [-d <json>]`. That API calls itself
//! experimental (spec `0.0.1`), so every call is best-effort. A caller can
//! always tell [`OpenCodeError::Unavailable`] - no `opencode`, no running
//! server, an older opencode that does not know the operation, or output this
//! client cannot parse - from [`OpenCodeError::Rejected`], where the server
//! answered and refused. That distinction is the contract: an unreachable
//! opencode must let a Run fall back rather than fail it.
//!
//! Every response is mapped onto typed values ([`Session`], [`SessionOutcome`])
//! so callers never thread raw JSON around. Tests drive the client through the
//! [`OpenCodeRunner`] seam, so parsing and the unavailable path are covered
//! without a live server.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::engine::WorkerId;

/// The opencode executable looked up on `PATH`.
const OPENCODE_EXECUTABLE: &str = "opencode";

/// The environment variable opencode injects into a session's shell commands,
/// naming the session that ran them.
pub(crate) const OPENCODE_SESSION_ENV: &str = "OPENCODE_SESSION_ID";

/// The opencode session that dispatched this process, when it runs inside one.
///
/// opencode sets `OPENCODE_SESSION_ID` in the environment of every command a
/// session runs, so a `jjfx dispatch` issued from an agent's shell knows which
/// session asked for the work. A Dispatch from outside opencode - the TUI, a
/// terminal, CI - carries no session, and its Workers stay root sessions.
pub(crate) fn dispatching_session() -> Option<String> {
    session_from_env(std::env::var(OPENCODE_SESSION_ENV).ok().as_deref())
}

/// Normalizes the environment value: a blank or absent variable names no
/// session.
fn session_from_env(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
        .map(str::to_owned)
}

/// A session as opencode reports it.
///
/// Only the fields jjfx's Run path needs are kept: the id, the session that
/// owns it when it is a child, the directory the session is anchored to, how it
/// ended, and its title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    id: String,
    parent_id: Option<String>,
    directory: Option<PathBuf>,
    outcome: Option<SessionOutcome>,
    title: Option<String>,
}

impl Session {
    /// The session id, e.g. `ses_ec5f7b702ffe...`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The session that owns this one, or `None` for a root session.
    ///
    /// A child session is a session with a `parentID`; opencode has no
    /// `session.children` route, so a parent finds its children by filtering
    /// `session.list` on this field.
    pub fn parent_id(&self) -> Option<&str> {
        self.parent_id.as_deref()
    }

    /// The directory the session is anchored to (`location.directory`).
    ///
    /// `None` when opencode did not report one: a best-effort client tolerates
    /// a missing location rather than discarding the whole session.
    pub fn directory(&self) -> Option<&Path> {
        self.directory.as_deref()
    }

    /// How the session ended, once it has. `None` while it is still open.
    pub fn outcome(&self) -> Option<&SessionOutcome> {
        self.outcome.as_ref()
    }

    /// The session's human title, when opencode reported one.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }
}

/// How a session ended, as `Session.Info.outcome` reports it.
///
/// The API is experimental, so an outcome this client does not know is kept as
/// [`SessionOutcome::Unknown`] rather than treated as a parse failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionOutcome {
    /// The session finished its work.
    Succeeded,
    /// The session ended in failure.
    Failed,
    /// The session was interrupted.
    Interrupted,
    /// An outcome newer than this client. The wire value is preserved.
    Unknown(String),
}

/// How a human answered a pending permission request (`Permission.Reply`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    /// Allow this one request.
    Once,
    /// Allow this request and remember the choice for later ones.
    Always,
    /// Refuse the request.
    Reject,
}

impl PermissionDecision {
    /// The wire value opencode expects.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Always => "always",
            Self::Reject => "reject",
        }
    }
}

/// A pending permission request opencode reports for a session
/// (`Permission.Request`).
///
/// Only the fields a human needs to decide are kept: the id to answer with, the
/// action being requested, the resources it touches, and the provider's own
/// message when it supplies one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequest {
    id: String,
    action: String,
    resources: Vec<String>,
    message: Option<String>,
}

impl PermissionRequest {
    /// The request id to answer with, e.g. `per_ab12...`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The action the agent is asking to perform.
    pub fn action(&self) -> &str {
        &self.action
    }

    /// The resources the action touches, in provider order.
    pub fn resources(&self) -> &[String] {
        &self.resources
    }

    /// The provider's own explanatory message, when it supplies one.
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }
}

/// The model a session should run with (`Model.Ref`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRef {
    id: String,
    provider_id: String,
    variant: Option<String>,
}

impl ModelRef {
    /// Selects a model by id and provider.
    pub fn new(id: impl Into<String>, provider_id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            provider_id: provider_id.into(),
            variant: None,
        }
    }

    /// Selects a named variant of the model.
    pub fn with_variant(mut self, variant: impl Into<String>) -> Self {
        self.variant = Some(variant.into());
        self
    }
}

/// A `session.create` request.
///
/// `location.directory` is required: a Run's session must be anchored to the
/// Worker Workspace it runs in. `title`, `agent`, `model` and `parentID` are
/// optional and omitted from the request body when unset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    directory: PathBuf,
    title: Option<String>,
    agent: Option<String>,
    model: Option<ModelRef>,
    parent_id: Option<String>,
}

impl NewSession {
    /// Anchors the new session to `directory`.
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            title: None,
            agent: None,
            model: None,
            parent_id: None,
        }
    }

    /// Sets the session title.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Selects the agent the session runs.
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    /// Selects the model the session runs.
    pub fn with_model(mut self, model: ModelRef) -> Self {
        self.model = Some(model);
        self
    }

    /// Creates the session as a child of `parent_id`.
    ///
    /// A session without a parent is a root session, which is what a Dispatch
    /// that no opencode session owns produces.
    pub fn with_parent(mut self, parent_id: impl Into<String>) -> Self {
        self.parent_id = Some(parent_id.into());
        self
    }

    /// The directory the session is anchored to.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn body(&self) -> Result<String, OpenCodeError> {
        let body = CreateSessionBody {
            location: LocationBody {
                directory: &self.directory,
            },
            title: self.title.as_deref(),
            agent: self.agent.as_deref(),
            model: self.model.as_ref().map(ModelBody::from),
            parent_id: self.parent_id.as_deref(),
        };
        serde_json::to_string(&body).map_err(|source| OpenCodeError::Unavailable {
            reason: format!("cannot encode session.create request: {source}"),
        })
    }
}

#[derive(Serialize)]
struct CreateSessionBody<'a> {
    location: LocationBody<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<ModelBody<'a>>,
    #[serde(rename = "parentID", skip_serializing_if = "Option::is_none")]
    parent_id: Option<&'a str>,
}

#[derive(Serialize)]
struct LocationBody<'a> {
    directory: &'a Path,
}

#[derive(Serialize)]
struct ModelBody<'a> {
    id: &'a str,
    #[serde(rename = "providerID")]
    provider_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    variant: Option<&'a str>,
}

impl<'a> From<&'a ModelRef> for ModelBody<'a> {
    fn from(model: &'a ModelRef) -> Self {
        Self {
            id: &model.id,
            provider_id: &model.provider_id,
            variant: model.variant.as_deref(),
        }
    }
}

/// The body of a `session.prompt` request.
#[derive(Serialize)]
struct PromptBody<'a> {
    text: &'a str,
}

/// The body of a `session.permission.reply` request.
#[derive(Serialize)]
struct PermissionReplyBody {
    decision: &'static str,
}

/// The raw result of one `opencode api` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl CommandOutput {
    /// Builds an output. `exit_code` is `None` when a signal terminated the
    /// process.
    pub fn new(
        exit_code: Option<i32>,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
    ) -> Self {
        Self {
            exit_code,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    /// Whether the process exited successfully.
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }

    /// The process exit code, or `None` when a signal terminated it.
    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    /// Everything the process wrote to stdout.
    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    /// Everything the process wrote to stderr.
    pub fn stderr(&self) -> &str {
        &self.stderr
    }
}

/// Runs one `opencode api` invocation and returns its raw output.
///
/// This is the client's only seam. Production runs the `opencode` binary
/// ([`ProcessOpenCodeRunner`]); tests substitute canned output, so both parsing
/// and the unavailable path are exercised without a live server.
pub trait OpenCodeRunner: Send + Sync {
    /// Runs `opencode` with `args`. An `Err` means the process could not be
    /// started at all - typically the executable is not on `PATH`.
    fn run(&self, args: &[String]) -> io::Result<CommandOutput>;
}

/// The production runner: the `opencode` executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOpenCodeRunner {
    executable: PathBuf,
}

impl ProcessOpenCodeRunner {
    /// The runner that invokes `opencode` on `PATH`.
    pub fn new() -> Self {
        Self {
            executable: PathBuf::from(OPENCODE_EXECUTABLE),
        }
    }

    /// The runner that invokes an explicit `opencode` executable.
    pub fn with_executable(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }
}

impl Default for ProcessOpenCodeRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenCodeRunner for ProcessOpenCodeRunner {
    fn run(&self, args: &[String]) -> io::Result<CommandOutput> {
        let output = Command::new(&self.executable).args(args).output()?;
        Ok(CommandOutput::new(
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }
}

/// Why an opencode API call did not produce a value.
///
/// The two variants are the point of the type: callers fall back on
/// [`OpenCodeError::Unavailable`] and surface [`OpenCodeError::Rejected`].
#[derive(Debug, Error)]
pub enum OpenCodeError {
    /// opencode could not be reached: no executable, no running server, an
    /// older opencode that does not know the operation, a request this client
    /// could not encode, or output it could not parse.
    ///
    /// A Run must never fail on this.
    #[error("opencode API unavailable: {reason}")]
    Unavailable {
        /// What prevented the call, for logs and fallback messages.
        reason: String,
    },
    /// The server answered and rejected the request.
    #[error(
        "opencode rejected the request ({status}): {message}",
        status = status.map_or_else(|| "no HTTP status".to_owned(), |status| format!("HTTP {status}"))
    )]
    Rejected {
        /// The HTTP status the server answered with, when the CLI reported one.
        status: Option<u16>,
        /// The server's own error message.
        message: String,
    },
}

impl OpenCodeError {
    /// Whether this is unavailability rather than a server rejection.
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable { .. })
    }

    /// The HTTP status the server answered with, when it answered at all.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Rejected { status, .. } => *status,
            Self::Unavailable { .. } => None,
        }
    }
}

/// A best-effort, typed client for opencode's server API.
#[derive(Debug, Clone)]
pub struct OpenCodeApi<R = ProcessOpenCodeRunner> {
    runner: R,
}

impl OpenCodeApi<ProcessOpenCodeRunner> {
    /// A client that drives `opencode` on `PATH`.
    pub fn new() -> Self {
        Self {
            runner: ProcessOpenCodeRunner::new(),
        }
    }

    /// A client that drives an explicit `opencode` executable.
    pub fn with_executable(executable: impl Into<PathBuf>) -> Self {
        Self {
            runner: ProcessOpenCodeRunner::with_executable(executable),
        }
    }
}

impl Default for OpenCodeApi<ProcessOpenCodeRunner> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: OpenCodeRunner> OpenCodeApi<R> {
    /// Builds a client over an explicit runner. Tests use this seam.
    pub fn with_runner(runner: R) -> Self {
        Self { runner }
    }

    /// Creates a session (`session.create`).
    pub fn create_session(&self, request: &NewSession) -> Result<Session, OpenCodeError> {
        let body = request.body()?;
        let raw = self.call(operation_args("session.create", &[], Some(&body)))?;
        parse_envelope::<SessionEnvelope>(&raw, "session.create")
            .map(|envelope| envelope.data.into_session())
    }

    /// Reads one session by id (`session.get`).
    pub fn get_session(&self, session_id: &str) -> Result<Session, OpenCodeError> {
        let raw = self.call(operation_args(
            "session.get",
            &[("sessionID", session_id)],
            None,
        ))?;
        parse_envelope::<SessionEnvelope>(&raw, "session.get")
            .map(|envelope| envelope.data.into_session())
    }

    /// Lists the sessions opencode reports (`session.list`).
    pub fn list_sessions(&self) -> Result<Vec<Session>, OpenCodeError> {
        let raw = self.call(operation_args("session.list", &[], None))?;
        parse_envelope::<SessionsEnvelope>(&raw, "session.list").map(|envelope| {
            envelope
                .data
                .into_iter()
                .map(SessionInfo::into_session)
                .collect()
        })
    }

    /// The sessions owned by `parent_id` (`session.list` filtered by parent).
    ///
    /// opencode has no `session.children` route, so a parent enumerates its
    /// children by filtering the session list. The filter is asked of the server
    /// (`parentID`) and applied again here, so an older opencode that ignores
    /// the query parameter cannot answer with unrelated sessions.
    pub fn child_sessions(&self, parent_id: &str) -> Result<Vec<Session>, OpenCodeError> {
        let raw = self.call(operation_args(
            "session.list",
            &[("parentID", parent_id)],
            None,
        ))?;
        parse_envelope::<SessionsEnvelope>(&raw, "session.list").map(|envelope| {
            envelope
                .data
                .into_iter()
                .map(SessionInfo::into_session)
                .filter(|session| session.parent_id() == Some(parent_id))
                .collect()
        })
    }

    /// The ids of the sessions opencode currently reports as active
    /// (`session.active`).
    ///
    /// "Active" means the session is open, not that it is executing: opencode
    /// lists a session here while it waits for input too. It is a guard for the
    /// title fallback in [`run_session_id`], not a liveness signal.
    pub fn active_sessions(&self) -> Result<BTreeSet<String>, OpenCodeError> {
        let raw = self.call(operation_args("session.active", &[], None))?;
        parse_envelope::<ActiveSessionsEnvelope>(&raw, "session.active")
            .map(|envelope| envelope.data.into_keys().collect())
    }

    /// Interrupts a session's active execution (`session.interrupt`),
    /// reporting whether anything was actually interrupted.
    pub fn interrupt_session(&self, session_id: &str) -> Result<bool, OpenCodeError> {
        let raw = self.call(operation_args(
            "session.interrupt",
            &[("sessionID", session_id)],
            None,
        ))?;
        parse_envelope::<InterruptEnvelope>(&raw, "session.interrupt")
            .map(|envelope| envelope.interrupted)
    }

    /// Sends `prompt` to a session without waiting for it (`session.prompt`).
    ///
    /// This is opencode's asynchronous prompt - the route the docs call
    /// `prompt_async`: the daemon admits the input and schedules the agent loop,
    /// so jjfx reaches a Run's session without spawning a client process. The
    /// response body carries no value a caller needs, so only acceptance is
    /// reported.
    pub fn prompt_async(&self, session_id: &str, prompt: &str) -> Result<(), OpenCodeError> {
        let body = PromptBody { text: prompt };
        let body = serde_json::to_string(&body).map_err(|source| OpenCodeError::Unavailable {
            reason: format!("cannot encode session.prompt request: {source}"),
        })?;
        self.call(operation_args(
            "session.prompt",
            &[("sessionID", session_id)],
            Some(&body),
        ))?;
        Ok(())
    }

    /// Answers a pending permission request (`session.permission.reply`).
    pub fn reply_permission(
        &self,
        session_id: &str,
        request_id: &str,
        decision: PermissionDecision,
    ) -> Result<(), OpenCodeError> {
        let body = PermissionReplyBody {
            decision: decision.as_str(),
        };
        let body = serde_json::to_string(&body).map_err(|source| OpenCodeError::Unavailable {
            reason: format!("cannot encode session.permission.reply request: {source}"),
        })?;
        self.call(operation_args(
            "session.permission.reply",
            &[("sessionID", session_id), ("requestID", request_id)],
            Some(&body),
        ))?;
        Ok(())
    }

    /// The pending permission requests owned by a session
    /// (`session.permission.list`).
    pub fn pending_permissions(
        &self,
        session_id: &str,
    ) -> Result<Vec<PermissionRequest>, OpenCodeError> {
        let raw = self.call(operation_args(
            "session.permission.list",
            &[("sessionID", session_id)],
            None,
        ))?;
        parse_envelope::<PermissionsEnvelope>(&raw, "session.permission.list").map(|envelope| {
            envelope
                .data
                .into_iter()
                .map(PermissionInfo::into_request)
                .collect()
        })
    }

    /// Runs one invocation and returns its stdout, or the classified failure.
    fn call(&self, args: Vec<String>) -> Result<String, OpenCodeError> {
        let output = self
            .runner
            .run(&args)
            .map_err(|source| OpenCodeError::Unavailable {
                reason: format!("cannot run {OPENCODE_EXECUTABLE}: {source}"),
            })?;
        if output.succeeded() {
            return Ok(output.stdout);
        }
        Err(classify_failure(&output))
    }
}

/// Creates the session a fresh opencode Run drives, anchored to `workspace`, and
/// confirms opencode stored that anchor.
///
/// This is the whole point of the client for a Run: instead of relying on the
/// spawned process's cwd to place the Run in its Worker Workspace, jjfx tells
/// opencode where to run. The session is created with `workspace` as its
/// `location.directory` and, once it exists, read back to confirm opencode did
/// not put it somewhere else.
///
/// `parent` is the opencode session the Dispatch was issued from, when there is
/// one ([`dispatching_session`]): the Run's session is then created as that
/// session's child, so the dispatching session can see it. `None` creates a
/// root session, which is what a Dispatch with no opencode session behind it
/// produces.
///
/// `Ok(Some(id))` is the session to drive. `Ok(None)` means the API is
/// unavailable - no executable, no running server, an older opencode without
/// these routes, or output this client cannot parse - and the caller must fall
/// back to today's invocation rather than fail the Run. `Err` means opencode
/// answered but the Run must not proceed: it refused to create the session, or
/// it anchored the session somewhere other than `workspace`, which would let the
/// Run commit into the wrong worktree.
pub(crate) fn create_run_session<R: OpenCodeRunner>(
    api: &OpenCodeApi<R>,
    workspace: &Path,
    title: Option<&str>,
    parent: Option<&str>,
) -> Result<Option<String>, String> {
    let mut request = NewSession::new(workspace);
    if let Some(title) = title.filter(|title| !title.is_empty()) {
        request = request.with_title(title);
    }
    if let Some(parent) = parent.filter(|parent| !parent.is_empty()) {
        request = request.with_parent(parent);
    }
    let created = match api.create_session(&request) {
        Ok(session) => session,
        Err(error) if error.is_unavailable() => return Ok(None),
        Err(error) => {
            return Err(format!(
                "opencode refused to create the Run's session: {error}"
            ));
        }
    };
    let session_id = created.id().to_owned();
    // Read the session back so the anchor opencode actually stored is checked,
    // not merely echoed from the request.
    match api.get_session(&session_id) {
        Ok(session) => match session.directory() {
            Some(directory) if !same_directory(directory, workspace) => Err(format!(
                "opencode anchored the Run's session to {} instead of the Worker Workspace {}",
                directory.display(),
                workspace.display()
            )),
            // A session opencode reported without a location cannot be checked,
            // and the assertion is best-effort: drive the session we created.
            _ => Ok(Some(session_id)),
        },
        // The session exists and its id is known, so a failed read-back must not
        // discard it: the assertion is best-effort.
        Err(error) if error.is_unavailable() => Ok(Some(session_id)),
        Err(error) => Err(format!(
            "cannot confirm the Run's opencode session location: {error}"
        )),
    }
}

/// The `title` jjfx sets on a Run's session, so a session jjfx did not create
/// can still be recognized by it.
pub(crate) fn run_session_title(worker: &WorkerId, ticket: &str) -> String {
    format!("pool:{worker}:{ticket}")
}

/// Identifies the opencode session a Worker's Run drives.
///
/// The id jjfx recorded when it created or resumed the Run's session is exact
/// and is preferred. Only when nothing was recorded - a session jjfx did not
/// create - does this fall back to matching the Run's title against the
/// sessions opencode reports as active. The fallback refuses to guess: an
/// absent or ambiguous match yields `None`, and an unreachable API yields
/// `None` rather than an error.
pub(crate) fn run_session_id<R: OpenCodeRunner>(
    api: &OpenCodeApi<R>,
    recorded: Option<&str>,
    title: Option<&str>,
) -> Option<String> {
    if let Some(session_id) = recorded.filter(|session_id| !session_id.is_empty()) {
        return Some(session_id.to_owned());
    }
    let title = title.filter(|title| !title.is_empty())?;
    match_active_session_by_title(api, title).ok().flatten()
}

/// Matches `title` against the sessions opencode reports as active, returning
/// the id only for a single unambiguous match.
///
/// A title is not a unique identity: a retried Run reuses it, and a finished
/// session keeps it. Restricting the match to active sessions keeps a stale
/// session out, and refusing an ambiguous match means a Run is never stopped
/// by a guess.
pub(crate) fn match_active_session_by_title<R: OpenCodeRunner>(
    api: &OpenCodeApi<R>,
    title: &str,
) -> Result<Option<String>, OpenCodeError> {
    let active = api.active_sessions()?;
    let mut matches = api
        .list_sessions()?
        .into_iter()
        .filter(|session| session.title() == Some(title) && active.contains(session.id()))
        .map(|session| session.id().to_owned());
    match (matches.next(), matches.next()) {
        (Some(session_id), None) => Ok(Some(session_id)),
        _ => Ok(None),
    }
}

/// Whether two directories name the same location, tolerating a symlinked or
/// otherwise non-canonical spelling on either side.
fn same_directory(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[derive(Deserialize)]
struct SessionEnvelope {
    data: SessionInfo,
}

#[derive(Deserialize)]
struct SessionsEnvelope {
    data: Vec<SessionInfo>,
}

#[derive(Deserialize)]
struct ActiveSessionsEnvelope {
    data: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct InterruptEnvelope {
    interrupted: bool,
}

#[derive(Deserialize)]
struct PermissionsEnvelope {
    data: Vec<PermissionInfo>,
}

#[derive(Deserialize)]
struct PermissionInfo {
    id: String,
    action: String,
    #[serde(default)]
    resources: Vec<String>,
    #[serde(default)]
    message: Option<String>,
}

impl PermissionInfo {
    fn into_request(self) -> PermissionRequest {
        PermissionRequest {
            id: self.id,
            action: self.action,
            resources: self.resources,
            message: self.message,
        }
    }
}

#[derive(Deserialize)]
struct SessionInfo {
    id: String,
    #[serde(rename = "parentID", default)]
    parent_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    outcome: Option<String>,
    #[serde(default)]
    location: Option<LocationRef>,
}

#[derive(Deserialize)]
struct LocationRef {
    directory: PathBuf,
}

impl SessionInfo {
    fn into_session(self) -> Session {
        Session {
            id: self.id,
            parent_id: self.parent_id,
            directory: self.location.map(|location| location.directory),
            outcome: self.outcome.map(session_outcome),
            title: self.title,
        }
    }
}

fn session_outcome(raw: String) -> SessionOutcome {
    match raw.as_str() {
        "succeeded" => SessionOutcome::Succeeded,
        "failed" => SessionOutcome::Failed,
        "interrupted" => SessionOutcome::Interrupted,
        _ => SessionOutcome::Unknown(raw),
    }
}

/// Parses one response envelope, treating anything unreadable as unavailability:
/// a best-effort client must not turn an older opencode's different shape into
/// a hard failure.
fn parse_envelope<T: DeserializeOwned>(raw: &str, operation: &str) -> Result<T, OpenCodeError> {
    serde_json::from_str(raw.trim()).map_err(|source| OpenCodeError::Unavailable {
        reason: format!("cannot parse opencode {operation} response: {source}"),
    })
}

/// Builds the arguments for `opencode api <operation> [--param k=v] [--data <json>]`.
fn operation_args(operation: &str, params: &[(&str, &str)], body: Option<&str>) -> Vec<String> {
    let mut args = vec!["api".to_owned(), operation.to_owned()];
    for (name, value) in params {
        args.push("--param".to_owned());
        args.push(format!("{name}={value}"));
    }
    if let Some(body) = body {
        args.push("--data".to_owned());
        args.push(body.to_owned());
    }
    args
}

/// Classifies a failed invocation: a server that answered is a rejection,
/// anything else is unavailability.
fn classify_failure(output: &CommandOutput) -> OpenCodeError {
    if let Some(status) = http_status(&output.stderr) {
        return OpenCodeError::Rejected {
            status: Some(status),
            message: rejection_message(&output.stdout, &output.stderr),
        };
    }
    // The CLI prints the response body to stdout and the status line to stderr.
    // Only the body is guaranteed, so an error body without a status line is
    // still a rejection.
    if is_error_body(&output.stdout) {
        return OpenCodeError::Rejected {
            status: None,
            message: rejection_message(&output.stdout, &output.stderr),
        };
    }
    OpenCodeError::Unavailable {
        reason: failure_reason(output),
    }
}

/// The HTTP status the CLI prints to stderr as `HTTP <code> <reason>`, which it
/// does exactly when a server answered the request. Its absence means the
/// request never reached a server - the case callers must treat as
/// "unavailable".
fn http_status(stderr: &str) -> Option<u16> {
    stderr.lines().find_map(|line| {
        line.trim()
            .strip_prefix("HTTP ")
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|code| code.parse().ok())
    })
}

/// Whether stdout carries an opencode error body rather than a response.
fn is_error_body(stdout: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout.trim()) else {
        return false;
    };
    value.get("_tag").is_some() || value.get("message").is_some()
}

/// The message to report for a rejection: the server's own, else the body, else
/// the CLI's stderr.
fn rejection_message(stdout: &str, stderr: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout.trim())
        && let Some(message) = value.get("message").and_then(serde_json::Value::as_str)
    {
        return message.to_owned();
    }
    let stdout = stdout.trim();
    if !stdout.is_empty() {
        return stdout.to_owned();
    }
    let stderr = stderr.trim();
    if stderr.is_empty() {
        "opencode api failed without an error message".to_owned()
    } else {
        stderr.to_owned()
    }
}

/// The reason to report for an unavailable call: the CLI's first stderr line,
/// else the exit status.
fn failure_reason(output: &CommandOutput) -> String {
    let stderr = output.stderr.trim();
    if !stderr.is_empty() {
        return first_line(stderr).to_owned();
    }
    match output.exit_code {
        Some(code) => format!("opencode api exited with status {code}"),
        None => "opencode api was terminated by a signal".to_owned(),
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("").trim()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;

    /// A runner that returns canned outputs and records the arguments it saw.
    /// This is the seam that lets parsing and the unavailable path be tested
    /// without a live opencode server.
    struct FakeRunner {
        replies: Mutex<VecDeque<io::Result<CommandOutput>>>,
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl FakeRunner {
        fn replying(replies: Vec<io::Result<CommandOutput>>) -> Self {
            Self {
                replies: Mutex::new(replies.into()),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    impl OpenCodeRunner for FakeRunner {
        fn run(&self, args: &[String]) -> io::Result<CommandOutput> {
            self.calls.lock().expect("calls lock").push(args.to_vec());
            self.replies
                .lock()
                .expect("replies lock")
                .pop_front()
                .expect("one canned reply per call")
        }
    }

    fn ok(body: &str) -> io::Result<CommandOutput> {
        Ok(CommandOutput::new(Some(0), body, ""))
    }

    fn failed(exit_code: i32, stdout: &str, stderr: &str) -> io::Result<CommandOutput> {
        Ok(CommandOutput::new(Some(exit_code), stdout, stderr))
    }

    /// A realistic `session.list` payload, including the fields jjfx ignores.
    const SESSIONS: &str = r#"{"data":[
        {"id":"ses_one","projectID":"p","cost":0.5,
         "tokens":{"input":1,"output":2,"reasoning":0,"cache":{"read":0,"write":0}},
         "time":{"created":1,"updated":2},"title":"Ticket 39 client",
         "outcome":"succeeded","location":{"directory":"/w/one"}},
        {"id":"ses_two","projectID":"p","cost":0,
         "tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},
         "time":{"created":3,"updated":4},"location":{"directory":"/w/two"}}
    ]}"#;

    #[test]
    fn list_sessions_maps_the_payload_onto_typed_sessions() {
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![ok(SESSIONS)]));

        let sessions = api.list_sessions().expect("parses");

        assert_eq!(api.runner.calls(), vec![vec!["api", "session.list"]]);
        assert_eq!(sessions.len(), 2);

        assert_eq!(sessions[0].id(), "ses_one");
        assert_eq!(sessions[0].directory(), Some(Path::new("/w/one")));
        assert_eq!(sessions[0].title(), Some("Ticket 39 client"));
        assert_eq!(sessions[0].outcome(), Some(&SessionOutcome::Succeeded));

        // An open session has no outcome and no title, and that is not a
        // failure: both stay absent rather than defaulting to something.
        assert_eq!(sessions[1].id(), "ses_two");
        assert_eq!(sessions[1].directory(), Some(Path::new("/w/two")));
        assert_eq!(sessions[1].title(), None);
        assert_eq!(sessions[1].outcome(), None);
    }

    #[test]
    fn create_session_sends_location_and_parses_the_created_session() {
        let created = r#"{"data":{"id":"ses_new","projectID":"p","cost":0,
            "tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},
            "time":{"created":1,"updated":1},"title":"Run","location":{"directory":"/w/new"}}}"#;
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![ok(created)]));

        let request = NewSession::new("/w/new")
            .with_title("Run")
            .with_agent("build");
        let session = api.create_session(&request).expect("creates");

        assert_eq!(session.id(), "ses_new");
        assert_eq!(session.directory(), Some(Path::new("/w/new")));

        let calls = api.runner.calls();
        assert_eq!(calls.len(), 1);
        let args = &calls[0];
        assert_eq!(&args[..2], ["api", "session.create"]);
        let body = args
            .windows(2)
            .find(|pair| pair[0] == "--data")
            .map(|pair| pair[1].clone())
            .expect("request carries a body");
        let body: serde_json::Value = serde_json::from_str(&body).expect("valid JSON body");
        assert_eq!(body["location"]["directory"], "/w/new");
        assert_eq!(body["title"], "Run");
        assert_eq!(body["agent"], "build");
        // Unset fields are omitted, not sent as null.
        assert!(body.get("model").is_none());
    }

    #[test]
    fn create_session_serializes_a_selected_model() {
        let created = r#"{"data":{"id":"ses_new","projectID":"p","cost":0,
            "tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},
            "time":{"created":1,"updated":1},"location":{"directory":"/w/new"}}}"#;
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![ok(created)]));

        let request = NewSession::new("/w/new")
            .with_model(ModelRef::new("gpt-5", "openai").with_variant("high"));
        api.create_session(&request).expect("creates");

        let calls = api.runner.calls();
        let body = calls[0]
            .windows(2)
            .find(|pair| pair[0] == "--data")
            .map(|pair| pair[1].clone())
            .expect("request carries a body");
        let body: serde_json::Value = serde_json::from_str(&body).expect("valid JSON body");
        assert_eq!(body["model"]["id"], "gpt-5");
        assert_eq!(body["model"]["providerID"], "openai");
        assert_eq!(body["model"]["variant"], "high");
    }

    #[test]
    fn get_session_addresses_the_session_by_path_parameter() {
        let session = r#"{"data":{"id":"ses_x","projectID":"p","cost":0,
            "tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},
            "time":{"created":1,"updated":1},"location":{"directory":"/w"}}}"#;
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![ok(session)]));

        let found = api.get_session("ses_x").expect("reads");

        assert_eq!(found.id(), "ses_x");
        assert_eq!(
            api.runner.calls(),
            vec![vec!["api", "session.get", "--param", "sessionID=ses_x"]]
        );
    }

    #[test]
    fn interrupt_session_reports_whether_anything_was_interrupted() {
        let api =
            OpenCodeApi::with_runner(FakeRunner::replying(vec![ok(r#"{"interrupted":false}"#)]));

        assert!(!api.interrupt_session("ses_x").expect("interrupts"));
        assert_eq!(
            api.runner.calls(),
            vec![vec![
                "api",
                "session.interrupt",
                "--param",
                "sessionID=ses_x"
            ]]
        );
    }

    #[test]
    fn prompt_async_addresses_the_session_and_sends_the_text() {
        let api =
            OpenCodeApi::with_runner(FakeRunner::replying(vec![ok(r#"{"data":{"id":"msg_1"}}"#)]));

        api.prompt_async("ses_x", "do the follow-up")
            .expect("prompts");

        let calls = api.runner.calls();
        assert_eq!(calls.len(), 1);
        let args = &calls[0];
        assert_eq!(
            &args[..4],
            ["api", "session.prompt", "--param", "sessionID=ses_x"]
        );
        let body = args
            .windows(2)
            .find(|pair| pair[0] == "--data")
            .map(|pair| pair[1].clone())
            .expect("request carries a body");
        let body: serde_json::Value = serde_json::from_str(&body).expect("valid JSON body");
        assert_eq!(body["text"], "do the follow-up");
    }

    #[test]
    fn prompt_async_is_unavailable_when_the_server_cannot_be_reached() {
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![failed(
            1,
            "",
            "Error: Could not reach server at http://127.0.0.1:1\n    at cli.api\n",
        )]));

        let error = api
            .prompt_async("ses_x", "do the follow-up")
            .expect_err("no server");

        assert!(error.is_unavailable());
    }

    #[test]
    fn pending_permissions_maps_the_payload_onto_typed_requests() {
        let payload = r#"{"data":[
            {"id":"per_1","sessionID":"ses_x","action":"bash",
             "resources":["rm -rf build"],"message":"run a shell command"},
            {"id":"per_2","sessionID":"ses_x","action":"edit","resources":["src/main.rs"]}
        ]}"#;
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![ok(payload)]));

        let pending = api.pending_permissions("ses_x").expect("parses");

        assert_eq!(
            api.runner.calls(),
            vec![vec![
                "api",
                "session.permission.list",
                "--param",
                "sessionID=ses_x"
            ]]
        );
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].id(), "per_1");
        assert_eq!(pending[0].action(), "bash");
        assert_eq!(pending[0].resources(), ["rm -rf build"]);
        assert_eq!(pending[0].message(), Some("run a shell command"));
        // A request without a message and without resources is not a failure.
        assert_eq!(pending[1].id(), "per_2");
        assert_eq!(pending[1].resources(), ["src/main.rs"]);
        assert_eq!(pending[1].message(), None);
    }

    #[test]
    fn reply_permission_addresses_the_request_and_sends_the_decision() {
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![ok("")]));

        api.reply_permission("ses_x", "per_1", PermissionDecision::Always)
            .expect("replies");

        let calls = api.runner.calls();
        assert_eq!(calls.len(), 1);
        let args = &calls[0];
        assert_eq!(&args[..2], ["api", "session.permission.reply"]);
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--param", "sessionID=ses_x"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--param", "requestID=per_1"])
        );
        let body = args
            .windows(2)
            .find(|pair| pair[0] == "--data")
            .map(|pair| pair[1].clone())
            .expect("request carries a body");
        let body: serde_json::Value = serde_json::from_str(&body).expect("valid JSON body");
        assert_eq!(body["decision"], "always");
    }

    #[test]
    fn reply_permission_surfaces_a_server_rejection() {
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![failed(
            1,
            r#"{"_tag":"PermissionNotFoundError","message":"Permission not found: per_nope"}"#,
            "HTTP 404 Not Found\n",
        )]));

        let error = api
            .reply_permission("ses_x", "per_nope", PermissionDecision::Once)
            .expect_err("server rejects");

        assert!(!error.is_unavailable());
        assert_eq!(error.status(), Some(404));
        assert!(error.to_string().contains("Permission not found: per_nope"));
    }

    #[test]
    fn an_unknown_outcome_is_preserved_rather_than_failing_the_parse() {
        let payload = r#"{"data":[{"id":"ses_one","projectID":"p","cost":0,
            "tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},
            "time":{"created":1,"updated":2},"outcome":"cancelled",
            "location":{"directory":"/w"}}]}"#;
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![ok(payload)]));

        let sessions = api.list_sessions().expect("parses");

        assert_eq!(
            sessions[0].outcome(),
            Some(&SessionOutcome::Unknown("cancelled".to_owned()))
        );
    }

    #[test]
    fn a_missing_executable_is_unavailable() {
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![Err(io::Error::from(
            io::ErrorKind::NotFound,
        ))]));

        let error = api.list_sessions().expect_err("no opencode on PATH");

        assert!(error.is_unavailable());
        assert_eq!(error.status(), None);
    }

    #[test]
    fn an_older_opencode_that_lacks_the_operation_is_unavailable() {
        // `opencode api does.not.exist` prints an Effect stack trace to stderr
        // and nothing to stdout, so there is no server response to report.
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![failed(
            1,
            "",
            "UnknownError: An error occurred in Effect.tryPromise\n    at <anonymous>\n",
        )]));

        let error = api.list_sessions().expect_err("unknown operation");

        assert!(error.is_unavailable());
        assert!(error.to_string().contains("UnknownError"));
    }

    #[test]
    fn a_missing_server_is_unavailable() {
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![failed(
            1,
            "",
            "Error: Could not reach server at http://127.0.0.1:1\n    at cli.api\n",
        )]));

        let error = api.list_sessions().expect_err("no server");

        assert!(error.is_unavailable());
    }

    #[test]
    fn unparseable_success_output_is_unavailable() {
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![ok("not json at all")]));

        let error = api.list_sessions().expect_err("unreadable output");

        assert!(error.is_unavailable());
        assert!(error.to_string().contains("cannot parse"));
    }

    #[test]
    fn a_server_rejection_is_rejected_and_carries_the_status() {
        // `opencode api` prints the error body to stdout and `HTTP <status>` to
        // stderr when a server answered and refused.
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![failed(
            1,
            r#"{"_tag":"SessionNotFoundError","sessionID":"ses_nope","message":"Session not found: ses_nope"}"#,
            "HTTP 404 Not Found\n",
        )]));

        let error = api.get_session("ses_nope").expect_err("server rejects");

        assert!(!error.is_unavailable());
        assert_eq!(error.status(), Some(404));
        assert!(error.to_string().contains("Session not found: ses_nope"));
        assert!(error.to_string().contains("HTTP 404"));
    }

    #[test]
    fn an_error_body_without_a_status_line_is_still_rejected() {
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![failed(
            1,
            r#"{"_tag":"SessionNotFoundError","message":"Session not found: ses_nope"}"#,
            "",
        )]));

        let error = api.get_session("ses_nope").expect_err("server rejects");

        assert!(!error.is_unavailable());
        assert_eq!(error.status(), None);
        assert!(error.to_string().contains("Session not found: ses_nope"));
    }

    fn session_body(id: &str, directory: &str) -> String {
        format!(
            r#"{{"data":{{"id":"{id}","projectID":"p","cost":0,
            "tokens":{{"input":0,"output":0,"reasoning":0,"cache":{{"read":0,"write":0}}}},
            "time":{{"created":1,"updated":1}},"location":{{"directory":"{directory}"}}}}}}"#
        )
    }

    #[test]
    fn create_run_session_anchors_to_the_workspace_and_confirms_it() {
        // The created session and the read-back both report the Worker Workspace.
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![
            ok(&session_body("ses_new", "/w/new")),
            ok(&session_body("ses_new", "/w/new")),
        ]));

        let session_id = create_run_session(
            &api,
            Path::new("/w/new"),
            Some("pool:worker-01:ENG-42"),
            Some("ses_dispatch"),
        )
        .expect("creates")
        .expect("drives the created session");

        assert_eq!(session_id, "ses_new");

        let calls = api.runner.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(&calls[0][..2], ["api", "session.create"]);
        let body = calls[0]
            .windows(2)
            .find(|pair| pair[0] == "--data")
            .map(|pair| pair[1].clone())
            .expect("request carries a body");
        let body: serde_json::Value = serde_json::from_str(&body).expect("valid JSON body");
        assert_eq!(body["location"]["directory"], "/w/new");
        assert_eq!(body["title"], "pool:worker-01:ENG-42");
        // The dispatching session owns the Run's session.
        assert_eq!(body["parentID"], "ses_dispatch");
        assert_eq!(
            calls[1],
            vec!["api", "session.get", "--param", "sessionID=ses_new"]
        );
    }

    #[test]
    fn create_run_session_stays_a_root_session_without_a_dispatching_one() {
        // No opencode session dispatched this Run - the TUI, a terminal, CI - so
        // the Run's session is a root session and no `parentID` is sent.
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![
            ok(&session_body("ses_new", "/w")),
            ok(&session_body("ses_new", "/w")),
        ]));

        create_run_session(&api, Path::new("/w"), None, None).expect("creates");

        let body = api.runner.calls()[0]
            .windows(2)
            .find(|pair| pair[0] == "--data")
            .map(|pair| pair[1].clone())
            .expect("request carries a body");
        let body: serde_json::Value = serde_json::from_str(&body).expect("valid JSON body");
        assert!(body.get("parentID").is_none());
    }

    #[test]
    fn create_run_session_ignores_a_blank_parent() {
        // An empty `OPENCODE_SESSION_ID` names no session, so it must not be
        // sent as a parent opencode would have to reject.
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![
            ok(&session_body("ses_new", "/w")),
            ok(&session_body("ses_new", "/w")),
        ]));

        create_run_session(&api, Path::new("/w"), None, Some("")).expect("creates");

        let body = api.runner.calls()[0]
            .windows(2)
            .find(|pair| pair[0] == "--data")
            .map(|pair| pair[1].clone())
            .expect("request carries a body");
        let body: serde_json::Value = serde_json::from_str(&body).expect("valid JSON body");
        assert!(body.get("parentID").is_none());
    }

    #[test]
    fn create_run_session_omits_a_blank_title() {
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![
            ok(&session_body("ses_new", "/w")),
            ok(&session_body("ses_new", "/w")),
        ]));

        create_run_session(&api, Path::new("/w"), None, None).expect("creates");

        let body = api.runner.calls()[0]
            .windows(2)
            .find(|pair| pair[0] == "--data")
            .map(|pair| pair[1].clone())
            .expect("request carries a body");
        let body: serde_json::Value = serde_json::from_str(&body).expect("valid JSON body");
        assert!(body.get("title").is_none());
    }

    #[test]
    fn create_run_session_falls_back_when_the_api_is_unavailable() {
        // No executable, no server, or an older opencode: the Run must fall back
        // to today's process-cwd invocation rather than fail.
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![Err(io::Error::from(
            io::ErrorKind::NotFound,
        ))]));

        let session =
            create_run_session(&api, Path::new("/w"), Some("title"), None).expect("falls back");

        assert_eq!(session, None);
        assert_eq!(api.runner.calls().len(), 1);
    }

    #[test]
    fn create_run_session_surfaces_a_server_refusal() {
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![failed(
            1,
            "",
            "HTTP 400 Bad Request\n",
        )]));

        let reason = create_run_session(&api, Path::new("/w"), None, None).expect_err("refused");

        assert!(reason.contains("refused"), "{reason}");
    }

    #[test]
    fn create_run_session_fails_when_the_anchor_differs() {
        // opencode answered and put the session somewhere else: proceeding would
        // let the Run commit into the wrong worktree.
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![
            ok(&session_body("ses_new", "/w/new")),
            ok(&session_body("ses_new", "/main")),
        ]));

        let reason = create_run_session(&api, Path::new("/w/new"), None, None)
            .expect_err("mismatch detected");

        assert!(reason.contains("/main"), "{reason}");
        assert!(reason.contains("/w/new"), "{reason}");
    }

    #[test]
    fn create_run_session_drives_the_session_when_the_read_back_is_unavailable() {
        // The session exists and its id is known; a failed read-back is
        // best-effort and must not discard it.
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![
            ok(&session_body("ses_new", "/w")),
            failed(1, "", "no server"),
        ]));

        let session = create_run_session(&api, Path::new("/w"), None, None).expect("drives");

        assert_eq!(session.as_deref(), Some("ses_new"));
    }

    #[test]
    fn create_run_session_tolerates_a_session_without_a_reported_location() {
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![
            ok(r#"{"data":{"id":"ses_new"}}"#),
            ok(r#"{"data":{"id":"ses_new"}}"#),
        ]));

        let session = create_run_session(&api, Path::new("/w"), None, None).expect("drives");

        assert_eq!(session.as_deref(), Some("ses_new"));
    }

    #[test]
    fn active_sessions_maps_the_payload_onto_ids() {
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![ok(
            r#"{"data":{"ses_a":{"type":"running"},"ses_b":{"type":"running"}}}"#,
        )]));

        let active = api.active_sessions().expect("parses");

        assert_eq!(api.runner.calls(), vec![vec!["api", "session.active"]]);
        assert_eq!(
            active,
            BTreeSet::from(["ses_a".to_owned(), "ses_b".to_owned()])
        );
    }

    #[test]
    fn run_session_id_prefers_the_recorded_id_without_asking_the_api() {
        // No canned replies: any API call would panic, proving the exact id wins.
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![]));

        let session = run_session_id(&api, Some("ses_recorded"), Some("pool:worker-01:ENG-42"));

        assert_eq!(session.as_deref(), Some("ses_recorded"));
        assert!(api.runner.calls().is_empty());
    }

    #[test]
    fn run_session_id_falls_back_to_a_unique_active_title_match() {
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![
            ok(r#"{"data":{"ses_live":{"type":"running"}}}"#),
            ok(&sessions_titled(&["ses_live"], "pool:worker-01:ENG-42")),
        ]));

        let session = run_session_id(&api, None, Some("pool:worker-01:ENG-42"));

        assert_eq!(session.as_deref(), Some("ses_live"));
    }

    #[test]
    fn run_session_id_ignores_a_title_match_that_is_not_active() {
        // A finished session keeps its title; interrupting it would be a stale
        // action, so a title match must also be active.
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![
            ok(r#"{"data":{}}"#),
            ok(&sessions_titled(&["ses_stale"], "pool:worker-01:ENG-42")),
        ]));

        let session = run_session_id(&api, None, Some("pool:worker-01:ENG-42"));

        assert_eq!(session, None);
    }

    #[test]
    fn run_session_id_refuses_an_ambiguous_title_match() {
        // A retried Run reuses the title: two active matches must never be
        // resolved by a guess.
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![
            ok(r#"{"data":{"ses_one":{"type":"running"},"ses_two":{"type":"running"}}}"#),
            ok(&sessions_titled(
                &["ses_one", "ses_two"],
                "pool:worker-01:ENG-42",
            )),
        ]));

        let session = run_session_id(&api, None, Some("pool:worker-01:ENG-42"));

        assert_eq!(session, None);
    }

    #[test]
    fn run_session_id_yields_nothing_when_the_api_is_unavailable() {
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![failed(1, "", "no server")]));

        let session = run_session_id(&api, None, Some("pool:worker-01:ENG-42"));

        assert_eq!(session, None);
    }

    #[test]
    fn run_session_id_yields_nothing_without_a_recorded_id_or_title() {
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![]));

        assert_eq!(run_session_id(&api, None, None), None);
        assert_eq!(run_session_id(&api, Some(""), Some("")), None);
        assert!(api.runner.calls().is_empty());
    }

    #[test]
    fn list_sessions_keeps_the_parent_a_child_session_names() {
        let payload = r#"{"data":[
            {"id":"ses_child","parentID":"ses_parent","location":{"directory":"/w"}},
            {"id":"ses_root","location":{"directory":"/w"}}
        ]}"#;
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![ok(payload)]));

        let sessions = api.list_sessions().expect("parses");

        assert_eq!(sessions[0].parent_id(), Some("ses_parent"));
        // A root session names no parent, which is not a parse failure.
        assert_eq!(sessions[1].parent_id(), None);
    }

    #[test]
    fn child_sessions_asks_for_the_parent_and_keeps_only_its_children() {
        // The server is asked to filter by parent; the filter is applied again
        // so an older opencode that ignores the parameter cannot answer with
        // unrelated sessions.
        let payload = r#"{"data":[
            {"id":"ses_child","parentID":"ses_parent","location":{"directory":"/w"}},
            {"id":"ses_other","parentID":"ses_someone_else","location":{"directory":"/w"}},
            {"id":"ses_root","location":{"directory":"/w"}}
        ]}"#;
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![ok(payload)]));

        let children = api.child_sessions("ses_parent").expect("parses");

        assert_eq!(
            api.runner.calls(),
            vec![vec![
                "api",
                "session.list",
                "--param",
                "parentID=ses_parent"
            ]]
        );
        let ids: Vec<&str> = children.iter().map(|session| session.id()).collect();
        assert_eq!(ids, ["ses_child"]);
    }

    #[test]
    fn child_sessions_is_unavailable_when_the_server_cannot_be_reached() {
        let api = OpenCodeApi::with_runner(FakeRunner::replying(vec![failed(1, "", "no server")]));

        let error = api.child_sessions("ses_parent").expect_err("no server");

        assert!(error.is_unavailable());
    }

    #[test]
    fn session_from_env_names_a_session_only_for_a_real_value() {
        assert_eq!(
            session_from_env(Some("ses_abc")),
            Some("ses_abc".to_owned())
        );
        assert_eq!(
            session_from_env(Some("  ses_abc  ")),
            Some("ses_abc".to_owned())
        );
        assert_eq!(session_from_env(Some("")), None);
        assert_eq!(session_from_env(Some("   ")), None);
        assert_eq!(session_from_env(None), None);
    }

    fn sessions_titled(ids: &[&str], title: &str) -> String {
        let entries = ids
            .iter()
            .map(|id| {
                format!(r#"{{"id":"{id}","title":"{title}","location":{{"directory":"/w"}}}}"#)
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(r#"{{"data":[{entries}]}}"#)
    }
}
