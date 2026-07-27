//! Read-only Worker Pool snapshots built over the compatible state repositories.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::{Loaded, Repository, WireAgent, WireStatus, WorkerId, WorkerState};

/// The Agent Runtime recorded for a Worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRuntime {
    Claude,
    Codex,
}
impl AgentRuntime {
    fn parse(value: &WireAgent) -> Option<Self> {
        match value.as_str() {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// The execution-capacity state recorded for a Worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerStatus {
    Idle,
    Busy,
    Done,
    Failed,
}
impl WorkerStatus {
    fn parse(value: &WireStatus) -> Option<Self> {
        match value.as_str() {
            "idle" => Some(Self::Idle),
            "busy" => Some(Self::Busy),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Busy => "busy",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

/// Presence of an optional field in a persisted compatibility document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistedField<T> {
    Missing,
    Null,
    Value(T),
}

/// A Worker reference from the pool manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerReference {
    worker_id: WorkerId,
    workspace: String,
}
impl WorkerReference {
    pub fn worker_id(&self) -> &WorkerId {
        &self.worker_id
    }
    pub fn workspace(&self) -> &str {
        &self.workspace
    }
}

/// The successfully parsed pool manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolSnapshot {
    size: i64,
    gh_repo: String,
    workers: Vec<WorkerReference>,
}
impl PoolSnapshot {
    /// Compatibility schema generation. Go pool files have no explicit version.
    pub fn version(&self) -> u8 {
        1
    }
    pub fn size(&self) -> i64 {
        self.size
    }
    pub fn gh_repo(&self) -> &str {
        &self.gh_repo
    }
    pub fn workers(&self) -> &[WorkerReference] {
        &self.workers
    }
}

/// A successfully parsed Worker document joined to its pool membership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerSnapshot {
    worker_id: WorkerId,
    alias: String,
    workspace: String,
    status: WorkerStatus,
    ticket: Option<String>,
    agent_runtime: Option<AgentRuntime>,
    started_at: Option<String>,
    completed_at: Option<String>,
    log_file: Option<String>,
    branch_name: Option<String>,
    error: Option<String>,
    exit_code: Option<i64>,
    pid: Option<u32>,
    process_alive: Option<bool>,
}
impl WorkerSnapshot {
    pub fn version(&self) -> u8 {
        1
    }
    pub fn worker_id(&self) -> &WorkerId {
        &self.worker_id
    }
    pub fn alias(&self) -> &str {
        &self.alias
    }
    pub fn workspace(&self) -> &str {
        &self.workspace
    }
    pub fn status(&self) -> WorkerStatus {
        self.status
    }
    pub fn ticket(&self) -> Option<&str> {
        self.ticket.as_deref()
    }
    pub fn ticket_presence(&self) -> PersistedField<&str> {
        self.ticket
            .as_deref()
            .map_or(PersistedField::Null, PersistedField::Value)
    }
    /// Go Worker state has no separate Run identifier.
    pub fn run(&self) -> Option<&str> {
        None
    }
    pub fn run_presence(&self) -> PersistedField<&str> {
        PersistedField::Missing
    }
    pub fn agent_runtime(&self) -> Option<AgentRuntime> {
        self.agent_runtime
    }
    pub fn agent_runtime_presence(&self) -> PersistedField<AgentRuntime> {
        self.agent_runtime
            .map_or(PersistedField::Null, PersistedField::Value)
    }
    pub fn started_at(&self) -> Option<&str> {
        self.started_at.as_deref()
    }
    pub fn started_at_presence(&self) -> PersistedField<&str> {
        self.started_at
            .as_deref()
            .map_or(PersistedField::Null, PersistedField::Value)
    }
    /// Uses Go's completion time as the latest persisted activity timestamp.
    pub fn last_activity_at(&self) -> Option<&str> {
        self.completed_at.as_deref().or(self.started_at.as_deref())
    }
    pub fn last_activity_at_presence(&self) -> PersistedField<&str> {
        self.last_activity_at()
            .map_or(PersistedField::Null, PersistedField::Value)
    }
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub fn error_presence(&self) -> PersistedField<&str> {
        self.error
            .as_deref()
            .map_or(PersistedField::Null, PersistedField::Value)
    }
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }
    pub fn completed_at(&self) -> Option<&str> {
        self.completed_at.as_deref()
    }
    pub fn log_file(&self) -> Option<&str> {
        self.log_file.as_deref()
    }
    pub fn branch_name(&self) -> Option<&str> {
        self.branch_name.as_deref()
    }
    pub fn exit_code(&self) -> Option<i64> {
        self.exit_code
    }
    pub fn has_dead_process(&self) -> bool {
        self.process_alive == Some(false)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotDiagnosticKind {
    MissingPool,
    MalformedPool,
    MissingWorker,
    MalformedWorker,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotDiagnostic {
    kind: SnapshotDiagnosticKind,
    path: PathBuf,
    worker_id: Option<WorkerId>,
    message: String,
}
impl SnapshotDiagnostic {
    pub fn kind(&self) -> SnapshotDiagnosticKind {
        self.kind
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn worker_id(&self) -> Option<&WorkerId> {
        self.worker_id.as_ref()
    }
    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerPoolSnapshot {
    pool: Option<PoolSnapshot>,
    workers: Vec<WorkerSnapshot>,
    diagnostics: Vec<SnapshotDiagnostic>,
}
impl WorkerPoolSnapshot {
    pub fn pool(&self) -> Option<&PoolSnapshot> {
        self.pool.as_ref()
    }
    pub fn workers(&self) -> &[WorkerSnapshot] {
        &self.workers
    }
    pub fn worker(&self, worker_id: &str) -> Option<&WorkerSnapshot> {
        self.workers
            .iter()
            .find(|worker| worker.worker_id.as_str() == worker_id)
    }
    pub fn diagnostics(&self) -> &[SnapshotDiagnostic] {
        &self.diagnostics
    }
    pub fn is_missing(&self) -> bool {
        self.pool.is_none()
            && self
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.kind == SnapshotDiagnosticKind::MissingPool)
    }
}

impl Repository {
    /// Reads the existing Go-compatible Worker Pool without changing any file.
    pub fn read_worker_pool_snapshot(&self) -> WorkerPoolSnapshot {
        let state = self.state_store();
        let mut diagnostics = Vec::new();
        let pool = match state.pool().load() {
            Ok(Loaded::Missing) => {
                diagnostics.push(diagnostic(
                    self.root(),
                    SnapshotDiagnosticKind::MissingPool,
                    None,
                    "pool state is absent",
                ));
                return WorkerPoolSnapshot {
                    pool: None,
                    workers: Vec::new(),
                    diagnostics,
                };
            }
            Ok(Loaded::Present(versioned)) => versioned.value,
            Err(error) => {
                diagnostics.push(diagnostic(
                    self.root(),
                    SnapshotDiagnosticKind::MalformedPool,
                    None,
                    error,
                ));
                return WorkerPoolSnapshot {
                    pool: None,
                    workers: Vec::new(),
                    diagnostics,
                };
            }
        };
        let references: Vec<_> = pool
            .workers
            .iter()
            .cloned()
            .map(|worker_id| WorkerReference {
                workspace: worker_id.as_str().to_owned(),
                worker_id,
            })
            .collect();
        let snapshot = PoolSnapshot {
            size: pool.size,
            gh_repo: pool.gh_repo.clone(),
            workers: references,
        };
        let mut workers = Vec::new();
        for worker_id in pool.workers {
            match state.worker(worker_id.clone()).load() {
                Ok(Loaded::Missing) => diagnostics.push(diagnostic(
                    self.root(),
                    SnapshotDiagnosticKind::MissingWorker,
                    Some(worker_id),
                    "Worker state is absent",
                )),
                Err(error) => diagnostics.push(diagnostic(
                    self.root(),
                    SnapshotDiagnosticKind::MalformedWorker,
                    Some(worker_id),
                    error,
                )),
                Ok(Loaded::Present(versioned)) => match worker_snapshot(
                    worker_id.clone(),
                    pool.names.get(&worker_id).cloned().unwrap_or_default(),
                    versioned.value,
                ) {
                    Ok(worker) => workers.push(worker),
                    Err(message) => diagnostics.push(diagnostic(
                        self.root(),
                        SnapshotDiagnosticKind::MalformedWorker,
                        Some(worker_id),
                        message,
                    )),
                },
            }
        }
        WorkerPoolSnapshot {
            pool: Some(snapshot),
            workers,
            diagnostics,
        }
    }
}

fn diagnostic(
    root: &Path,
    kind: SnapshotDiagnosticKind,
    worker_id: Option<WorkerId>,
    message: impl fmt::Display,
) -> SnapshotDiagnostic {
    SnapshotDiagnostic {
        kind,
        path: root.to_owned(),
        worker_id,
        message: message.to_string(),
    }
}

fn worker_snapshot(
    worker_id: WorkerId,
    alias: String,
    state: WorkerState,
) -> Result<WorkerSnapshot, String> {
    let status = WorkerStatus::parse(&state.status)
        .ok_or_else(|| format!("unknown Worker status {:?}", state.status.as_str()))?;
    let agent_runtime = state
        .agent
        .as_ref()
        .map(AgentRuntime::parse)
        .transpose_option()
        .ok_or_else(|| "unknown Agent Runtime".to_owned())?;
    let pid = state
        .pid
        .map(u32::try_from)
        .transpose()
        .map_err(|_| "Worker PID must fit a positive u32".to_owned())?;
    if pid == Some(0) {
        return Err("Worker PID must be greater than zero".to_owned());
    }
    Ok(WorkerSnapshot {
        workspace: worker_id.as_str().to_owned(),
        worker_id,
        alias,
        status,
        ticket: state.ticket,
        agent_runtime,
        started_at: state.started_at.map(|value| value.as_str().to_owned()),
        completed_at: state.completed_at.map(|value| value.as_str().to_owned()),
        log_file: state.log_file,
        branch_name: state.branch_name,
        error: state.error,
        exit_code: state.exit_code,
        process_alive: pid.map(process_is_alive),
        pid,
    })
}

trait TransposeOption<T> {
    fn transpose_option(self) -> Option<Option<T>>;
}
impl<T> TransposeOption<T> for Option<Option<T>> {
    fn transpose_option(self) -> Option<Option<T>> {
        match self {
            None => Some(None),
            Some(Some(value)) => Some(Some(value)),
            Some(None) => None,
        }
    }
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    use rustix::process::{Pid, test_kill_process};

    i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .is_some_and(|pid| test_kill_process(pid).is_ok())
}
#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    true
}
