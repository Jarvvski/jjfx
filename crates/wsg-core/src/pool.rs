//! Read-only Worker Pool snapshots.
//!
//! This module owns the compatibility seam for the persisted pool documents. It
//! turns one pool manifest plus its referenced Worker documents into an
//! immutable value and keeps malformed individual Workers from hiding healthy
//! ones. It never writes or reconciles state.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::Repository;

const POOL_FILE: &str = ".jj/pool.json";
const WORKER_DIRECTORY: &str = ".jj/workers";

fn deserialize_nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::<T>::deserialize(deserializer)?))
}

#[derive(Debug, Deserialize)]
struct PoolDocument {
    version: u8,
    workers: Vec<PoolWorkerDocument>,
}

#[derive(Debug, Deserialize)]
struct PoolWorkerDocument {
    worker_id: String,
    workspace: String,
}

#[derive(Debug, Deserialize)]
struct WorkerDocument {
    version: u8,
    worker_id: String,
    alias: String,
    workspace: String,
    status: String,
    #[serde(default, deserialize_with = "deserialize_nullable")]
    ticket: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_nullable")]
    run: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_nullable")]
    agent_runtime: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_nullable")]
    started_at: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_nullable")]
    last_activity_at: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_nullable")]
    error: Option<Option<String>>,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(flatten)]
    _extra: BTreeMap<String, Value>,
}

/// A stable Worker identifier from the pool manifest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkerId(String);

impl WorkerId {
    /// Creates an identifier from persisted Worker state.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the identifier as persisted.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The agent runtime recorded for a Worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRuntime {
    /// Claude Code.
    Claude,
    /// Codex.
    Codex,
}

impl AgentRuntime {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }

    /// Returns the compatibility spelling used in persisted state.
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
    /// The Worker is available for a Reservation.
    Idle,
    /// The Worker has an active Run.
    Busy,
    /// The Worker completed its latest Run.
    Done,
    /// The Worker failed its latest Run.
    Failed,
}

impl WorkerStatus {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "idle" => Some(Self::Idle),
            "busy" => Some(Self::Busy),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }

    /// Returns the compatibility spelling used in persisted state.
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
    /// The field was omitted.
    Missing,
    /// The field was present with an explicit JSON `null`.
    Null,
    /// The field was present with a value.
    Value(T),
}

impl<T> PersistedField<T> {
    fn as_ref(&self) -> PersistedField<&T> {
        match self {
            Self::Missing => PersistedField::Missing,
            Self::Null => PersistedField::Null,
            Self::Value(value) => PersistedField::Value(value),
        }
    }
}

/// A Worker reference from the pool manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerReference {
    worker_id: WorkerId,
    workspace: String,
}

impl WorkerReference {
    /// Returns the referenced Worker identifier.
    pub fn worker_id(&self) -> &WorkerId {
        &self.worker_id
    }

    /// Returns the Worker Workspace name.
    pub fn workspace(&self) -> &str {
        &self.workspace
    }
}

/// The successfully parsed pool manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolSnapshot {
    version: u8,
    workers: Vec<WorkerReference>,
}

impl PoolSnapshot {
    /// Returns the persisted pool schema version.
    pub fn version(&self) -> u8 {
        self.version
    }

    /// Returns Worker references in their persisted stable order.
    pub fn workers(&self) -> &[WorkerReference] {
        &self.workers
    }
}

/// A successfully parsed Worker document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerSnapshot {
    version: u8,
    worker_id: WorkerId,
    alias: String,
    workspace: String,
    status: WorkerStatus,
    ticket: PersistedField<String>,
    run: PersistedField<String>,
    agent_runtime: PersistedField<AgentRuntime>,
    started_at: PersistedField<String>,
    last_activity_at: PersistedField<String>,
    error: PersistedField<String>,
    pid: Option<u32>,
    process_alive: Option<bool>,
}

impl WorkerSnapshot {
    /// Returns the persisted Worker schema version.
    pub fn version(&self) -> u8 {
        self.version
    }

    /// Returns this Worker's stable identifier.
    pub fn worker_id(&self) -> &WorkerId {
        &self.worker_id
    }

    /// Returns the human-facing Worker alias.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// Returns the assigned Worker Workspace name.
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    /// Returns the persisted Worker status.
    pub fn status(&self) -> WorkerStatus {
        self.status
    }

    /// Returns the assigned Ticket, if any.
    pub fn ticket(&self) -> Option<&str> {
        match &self.ticket {
            PersistedField::Value(value) => Some(value),
            PersistedField::Missing | PersistedField::Null => None,
        }
    }

    /// Returns the persisted presence of the Ticket field.
    pub fn ticket_presence(&self) -> PersistedField<&str> {
        string_presence(&self.ticket)
    }

    /// Returns the active Run, if any.
    pub fn run(&self) -> Option<&str> {
        match &self.run {
            PersistedField::Value(value) => Some(value),
            PersistedField::Missing | PersistedField::Null => None,
        }
    }

    /// Returns the persisted presence of the Run field.
    pub fn run_presence(&self) -> PersistedField<&str> {
        string_presence(&self.run)
    }

    /// Returns the selected Agent Runtime, if known.
    pub fn agent_runtime(&self) -> Option<AgentRuntime> {
        match self.agent_runtime {
            PersistedField::Value(runtime) => Some(runtime),
            PersistedField::Missing | PersistedField::Null => None,
        }
    }

    /// Returns the persisted presence of the Agent Runtime field.
    pub fn agent_runtime_presence(&self) -> PersistedField<AgentRuntime> {
        self.agent_runtime.clone()
    }

    /// Returns the persisted Run start timestamp, if any.
    pub fn started_at(&self) -> Option<&str> {
        match &self.started_at {
            PersistedField::Value(value) => Some(value),
            PersistedField::Missing | PersistedField::Null => None,
        }
    }

    /// Returns the persisted presence of the Run start timestamp field.
    pub fn started_at_presence(&self) -> PersistedField<&str> {
        string_presence(&self.started_at)
    }

    /// Returns the persisted last-activity timestamp, if any.
    pub fn last_activity_at(&self) -> Option<&str> {
        match &self.last_activity_at {
            PersistedField::Value(value) => Some(value),
            PersistedField::Missing | PersistedField::Null => None,
        }
    }

    /// Returns the persisted presence of the last-activity timestamp field.
    pub fn last_activity_at_presence(&self) -> PersistedField<&str> {
        string_presence(&self.last_activity_at)
    }

    /// Returns the persisted error, if any.
    pub fn error(&self) -> Option<&str> {
        match &self.error {
            PersistedField::Value(value) => Some(value),
            PersistedField::Missing | PersistedField::Null => None,
        }
    }

    /// Returns the persisted presence of the error field.
    pub fn error_presence(&self) -> PersistedField<&str> {
        string_presence(&self.error)
    }

    /// Returns the recorded process identifier, if the document has one.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Reports whether a recorded process is no longer alive.
    ///
    /// A missing PID does not make a Worker stale because older compatible
    /// documents do not record one. This is derived only; the snapshot reader
    /// never rewrites Worker state.
    pub fn has_dead_process(&self) -> bool {
        self.process_alive == Some(false)
    }
}

/// The kind of issue encountered while reading a pool snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotDiagnosticKind {
    /// The pool manifest is absent.
    MissingPool,
    /// The pool manifest could not be parsed or read.
    MalformedPool,
    /// A referenced Worker document is absent.
    MissingWorker,
    /// A referenced Worker document could not be parsed or validated.
    MalformedWorker,
}

/// A non-fatal issue found while reading a pool snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotDiagnostic {
    kind: SnapshotDiagnosticKind,
    path: PathBuf,
    worker_id: Option<WorkerId>,
    message: String,
}

impl SnapshotDiagnostic {
    /// Returns the diagnostic category.
    pub fn kind(&self) -> SnapshotDiagnosticKind {
        self.kind
    }

    /// Returns the state path involved in the issue.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the referenced Worker, when this is a Worker diagnostic.
    pub fn worker_id(&self) -> Option<&WorkerId> {
        self.worker_id.as_ref()
    }

    /// Returns contextual detail suitable for a read-only status surface.
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// A complete immutable view of the persisted Worker Pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerPoolSnapshot {
    pool: Option<PoolSnapshot>,
    workers: Vec<WorkerSnapshot>,
    diagnostics: Vec<SnapshotDiagnostic>,
}

impl WorkerPoolSnapshot {
    /// Returns the pool manifest, or `None` when the pool file is absent or malformed.
    pub fn pool(&self) -> Option<&PoolSnapshot> {
        self.pool.as_ref()
    }

    /// Returns successfully parsed Workers in pool-manifest order.
    pub fn workers(&self) -> &[WorkerSnapshot] {
        &self.workers
    }

    /// Finds a successfully parsed Worker by identifier.
    pub fn worker(&self, worker_id: &str) -> Option<&WorkerSnapshot> {
        self.workers
            .iter()
            .find(|worker| worker.worker_id.as_str() == worker_id)
    }

    /// Returns all non-fatal missing or malformed state diagnostics.
    pub fn diagnostics(&self) -> &[SnapshotDiagnostic] {
        &self.diagnostics
    }

    /// Returns true when no pool state was available.
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
        read_snapshot(self.root())
    }
}

fn read_snapshot(root: &Path) -> WorkerPoolSnapshot {
    let pool_path = root.join(POOL_FILE);
    let mut diagnostics = Vec::new();
    let pool = match fs::read(&pool_path) {
        Ok(bytes) => match serde_json::from_slice::<PoolDocument>(&bytes) {
            Ok(document) => Some(document),
            Err(error) => {
                diagnostics.push(SnapshotDiagnostic {
                    kind: SnapshotDiagnosticKind::MalformedPool,
                    path: pool_path.clone(),
                    worker_id: None,
                    message: error.to_string(),
                });
                None
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            diagnostics.push(SnapshotDiagnostic {
                kind: SnapshotDiagnosticKind::MissingPool,
                path: pool_path.clone(),
                worker_id: None,
                message: "pool state is absent".to_string(),
            });
            return WorkerPoolSnapshot {
                pool: None,
                workers: Vec::new(),
                diagnostics,
            };
        }
        Err(error) => {
            diagnostics.push(SnapshotDiagnostic {
                kind: SnapshotDiagnosticKind::MalformedPool,
                path: pool_path.clone(),
                worker_id: None,
                message: error.to_string(),
            });
            None
        }
    };

    let Some(pool) = pool else {
        return WorkerPoolSnapshot {
            pool: None,
            workers: Vec::new(),
            diagnostics,
        };
    };

    if pool.version != 1 {
        diagnostics.push(SnapshotDiagnostic {
            kind: SnapshotDiagnosticKind::MalformedPool,
            path: pool_path,
            worker_id: None,
            message: format!("unsupported pool schema version {}", pool.version),
        });
        return WorkerPoolSnapshot {
            pool: None,
            workers: Vec::new(),
            diagnostics,
        };
    }

    let pool_snapshot = PoolSnapshot {
        version: pool.version,
        workers: pool
            .workers
            .iter()
            .map(|worker| WorkerReference {
                worker_id: WorkerId::new(worker.worker_id.clone()),
                workspace: worker.workspace.clone(),
            })
            .collect(),
    };
    let workers = pool
        .workers
        .into_iter()
        .filter_map(|reference| read_worker(root, reference, &mut diagnostics))
        .collect();

    WorkerPoolSnapshot {
        pool: Some(pool_snapshot),
        workers,
        diagnostics,
    }
}

fn read_worker(
    root: &Path,
    reference: PoolWorkerDocument,
    diagnostics: &mut Vec<SnapshotDiagnostic>,
) -> Option<WorkerSnapshot> {
    let worker_id = WorkerId::new(reference.worker_id);
    let path = match safe_worker_path(root, &worker_id) {
        Some(path) => path,
        None => {
            diagnostics.push(SnapshotDiagnostic {
                kind: SnapshotDiagnosticKind::MalformedWorker,
                path: root.join(WORKER_DIRECTORY),
                worker_id: Some(worker_id),
                message: "Worker identifier is not a safe file name".to_string(),
            });
            return None;
        }
    };
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            diagnostics.push(SnapshotDiagnostic {
                kind: SnapshotDiagnosticKind::MissingWorker,
                path,
                worker_id: Some(worker_id),
                message: "Worker state is absent".to_string(),
            });
            return None;
        }
        Err(error) => {
            diagnostics.push(SnapshotDiagnostic {
                kind: SnapshotDiagnosticKind::MalformedWorker,
                path,
                worker_id: Some(worker_id),
                message: error.to_string(),
            });
            return None;
        }
    };
    let document = match serde_json::from_slice::<WorkerDocument>(&bytes) {
        Ok(document) => document,
        Err(error) => {
            diagnostics.push(SnapshotDiagnostic {
                kind: SnapshotDiagnosticKind::MalformedWorker,
                path,
                worker_id: Some(worker_id),
                message: error.to_string(),
            });
            return None;
        }
    };
    if document.version != 1 {
        diagnostics.push(SnapshotDiagnostic {
            kind: SnapshotDiagnosticKind::MalformedWorker,
            path,
            worker_id: Some(worker_id),
            message: format!("unsupported Worker schema version {}", document.version),
        });
        return None;
    }
    if document.pid == Some(0) {
        diagnostics.push(SnapshotDiagnostic {
            kind: SnapshotDiagnosticKind::MalformedWorker,
            path,
            worker_id: Some(worker_id),
            message: "Worker PID must be greater than zero".to_string(),
        });
        return None;
    }

    let status = match WorkerStatus::parse(&document.status) {
        Some(status) => status,
        None => {
            diagnostics.push(SnapshotDiagnostic {
                kind: SnapshotDiagnosticKind::MalformedWorker,
                path,
                worker_id: Some(worker_id),
                message: format!("unknown Worker status {:?}", document.status),
            });
            return None;
        }
    };
    let agent_runtime = match document.agent_runtime {
        Some(Some(value)) => match AgentRuntime::parse(&value) {
            Some(runtime) => PersistedField::Value(runtime),
            None => {
                diagnostics.push(SnapshotDiagnostic {
                    kind: SnapshotDiagnosticKind::MalformedWorker,
                    path,
                    worker_id: Some(worker_id),
                    message: format!("unknown Agent Runtime {:?}", value),
                });
                return None;
            }
        },
        Some(None) => PersistedField::Null,
        None => PersistedField::Missing,
    };
    if document.worker_id != worker_id.as_str() || document.workspace != reference.workspace {
        diagnostics.push(SnapshotDiagnostic {
            kind: SnapshotDiagnosticKind::MalformedWorker,
            path,
            worker_id: Some(worker_id),
            message: "Worker identity does not match the pool reference".to_string(),
        });
        return None;
    }

    Some(WorkerSnapshot {
        version: document.version,
        worker_id,
        alias: document.alias,
        workspace: document.workspace,
        status,
        ticket: persisted_field(document.ticket),
        run: persisted_field(document.run),
        agent_runtime,
        started_at: persisted_field(document.started_at),
        last_activity_at: persisted_field(document.last_activity_at),
        error: persisted_field(document.error),
        process_alive: document.pid.map(process_is_alive),
        pid: document.pid,
    })
}

fn persisted_field<T>(value: Option<Option<T>>) -> PersistedField<T> {
    match value {
        None => PersistedField::Missing,
        Some(None) => PersistedField::Null,
        Some(Some(value)) => PersistedField::Value(value),
    }
}

fn string_presence(value: &PersistedField<String>) -> PersistedField<&str> {
    match value.as_ref() {
        PersistedField::Missing => PersistedField::Missing,
        PersistedField::Null => PersistedField::Null,
        PersistedField::Value(value) => PersistedField::Value(value.as_str()),
    }
}

fn safe_worker_path(root: &Path, worker_id: &WorkerId) -> Option<PathBuf> {
    let path = Path::new(worker_id.as_str());
    if path.components().count() == 1
        && matches!(
            path.components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        Some(
            root.join(WORKER_DIRECTORY)
                .join(format!("{worker_id}.json")),
        )
    } else {
        None
    }
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    pid != 0
        && std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success())
}

#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    true
}
