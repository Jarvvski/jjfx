//! Read-only Worker Pool snapshots built over the compatible state repositories.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use jiff::{RoundMode, Timestamp, TimestampRound, Unit};
use thiserror::Error;

use crate::{
    AgentRuntime, CommitOutcome, Expected, Loaded, PoolState, Repository, StateChange, StateError,
    StateRevision, WireStatus, WireTimestamp, WorkerId, WorkerState, WorkerWorkspaceError,
};

/// A positive number of reusable Worker slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PoolCapacity(usize);

impl PoolCapacity {
    /// Creates a capacity suitable for a Worker Pool.
    pub fn new(value: usize) -> Result<Self, PoolCapacityError> {
        if value == 0 {
            return Err(PoolCapacityError);
        }
        Ok(Self(value))
    }

    /// Returns the number of Worker slots.
    pub fn as_usize(self) -> usize {
        self.0
    }
}

/// An invalid Worker Pool capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("Worker Pool capacity must be greater than zero")]
pub struct PoolCapacityError;

/// The result of growing a Worker Pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolGrowth {
    capacity: PoolCapacity,
    added_workers: Vec<WorkerId>,
}

impl PoolGrowth {
    /// Returns the resulting pool capacity.
    pub fn capacity(&self) -> PoolCapacity {
        self.capacity
    }

    /// Returns Workers provisioned by this growth operation in pool order.
    pub fn added_workers(&self) -> &[WorkerId] {
        &self.added_workers
    }
}

/// Capacity reserved for one Ticket before an Agent Runtime starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    worker_id: WorkerId,
    ticket: String,
    agent_runtime: AgentRuntime,
    repository: Repository,
    worker_revision: StateRevision<WorkerState>,
}

pub(crate) enum PidPersistence {
    Persisted(StateRevision<WorkerState>),
    Missing,
    Conflict,
}

pub(crate) enum RunFinalization {
    Applied,
    Stale,
}

impl Reservation {
    /// Returns the Worker assigned to this Reservation.
    pub fn worker_id(&self) -> &WorkerId {
        &self.worker_id
    }

    /// Returns the Ticket assigned to this Reservation.
    pub fn ticket(&self) -> &str {
        &self.ticket
    }

    /// Returns the Agent Runtime persisted for this Run.
    pub fn agent_runtime(&self) -> AgentRuntime {
        self.agent_runtime
    }

    pub(crate) fn repository(&self) -> &Repository {
        &self.repository
    }

    pub(crate) fn worker_revision(&self) -> StateRevision<WorkerState> {
        self.worker_revision.clone()
    }

    pub(crate) fn persist_pid(&self, pid: u32) -> Result<PidPersistence, StateError> {
        let worker_state = self.repository.state_store().worker(self.worker_id.clone());
        let loaded = match worker_state.load()? {
            Loaded::Present(versioned) => versioned,
            Loaded::Missing => return Ok(PidPersistence::Missing),
        };
        let mut state = loaded.value;
        state.pid = Some(i64::from(pid));
        match worker_state.commit(
            Expected::Match(self.worker_revision.clone()),
            StateChange::Replace(state),
        )? {
            CommitOutcome::Applied(Loaded::Present(versioned)) => {
                Ok(PidPersistence::Persisted(versioned.revision().clone()))
            }
            CommitOutcome::Applied(Loaded::Missing) | CommitOutcome::Conflict(Loaded::Missing) => {
                Ok(PidPersistence::Missing)
            }
            CommitOutcome::Conflict(Loaded::Present(_)) => Ok(PidPersistence::Conflict),
        }
    }

    pub(crate) fn finalize(
        &self,
        revision: StateRevision<WorkerState>,
        exit_code: Option<i32>,
    ) -> Result<RunFinalization, WorkerPoolError> {
        let worker_state = self.repository.state_store().worker(self.worker_id.clone());
        let loaded = match worker_state.load()? {
            Loaded::Present(versioned) => versioned,
            Loaded::Missing => {
                return Err(WorkerPoolError::WorkerStateMissing {
                    worker: self.worker_id.clone(),
                });
            }
        };
        if loaded.value.status.as_str() != "busy" {
            return Ok(RunFinalization::Stale);
        }
        let mut state = loaded.value;
        let completed_at = current_timestamp()?;
        let successful = exit_code == Some(0);
        state.status = WireStatus::new(if successful { "done" } else { "failed" });
        state.completed_at = Some(completed_at);
        state.exit_code = exit_code.map(i64::from).or(Some(1));
        state.error = if successful {
            None
        } else {
            Some(match exit_code {
                Some(code) => format!("Run exited with code {code}"),
                None => "Run terminated by signal".to_owned(),
            })
        };
        match worker_state.commit(Expected::Match(revision), StateChange::Replace(state))? {
            CommitOutcome::Applied(_) => Ok(RunFinalization::Applied),
            CommitOutcome::Conflict(_) => Ok(RunFinalization::Stale),
        }
    }

    pub(crate) fn release(&self) -> Result<(), WorkerPoolError> {
        let worker_state = self.repository.state_store().worker(self.worker_id.clone());
        let loaded = match worker_state.load()? {
            Loaded::Present(versioned) => versioned,
            Loaded::Missing => {
                return Err(WorkerPoolError::WorkerStateMissing {
                    worker: self.worker_id.clone(),
                });
            }
        };
        let mut state = loaded.value;
        state.status = WireStatus::new("idle");
        state.agent = None;
        state.ticket = None;
        state.pid = None;
        state.started_at = None;
        state.completed_at = None;
        state.log_file = None;
        state.branch_name = None;
        state.exit_code = None;
        state.error = None;
        match worker_state.commit(
            Expected::Match(self.worker_revision.clone()),
            StateChange::Replace(state),
        )? {
            CommitOutcome::Applied(_) => Ok(()),
            CommitOutcome::Conflict(_) => Err(WorkerPoolError::ReleaseConflict {
                worker: self.worker_id.clone(),
            }),
        }
    }
}

/// Errors from Worker Pool creation, growth, and Reservation.
#[derive(Debug, Error)]
pub enum WorkerPoolError {
    #[error("cannot load Worker Pool state: {0}")]
    State(#[from] StateError),
    #[error("Worker Pool state has invalid size {0}")]
    InvalidSize(i64),
    #[error("cannot shrink Worker Pool from {current} to {requested}")]
    CannotShrink { current: usize, requested: usize },
    #[error("Worker Pool mutation conflicted with another process")]
    Conflict,
    #[error("no idle Worker is available for Ticket {ticket} (available: {available})")]
    NoIdleWorkers { ticket: String, available: usize },
    #[error("Worker {worker} is not a member of the Worker Pool")]
    WorkerNotInPool { worker: WorkerId },
    #[error("Worker {worker} is not idle")]
    WorkerNotIdle { worker: WorkerId },
    #[error("Worker {worker} state is missing")]
    WorkerStateMissing { worker: WorkerId },
    #[error("Worker {worker} changed before its Reservation could be released")]
    ReleaseConflict { worker: WorkerId },
    #[error("invalid configured Agent Runtime {value:?} (expected claude or codex)")]
    InvalidAgentRuntime { value: String },
    #[error("cannot discover GitHub repository: {0}")]
    RepositoryDiscovery(String),
    #[error("cannot create a Worker timestamp: {0}")]
    Timestamp(String),
    #[error("cannot generate a Worker ID: {0}")]
    WorkerId(String),
    #[error("cannot provision Worker {worker}: {source}")]
    Provision {
        worker: WorkerId,
        source: WorkerWorkspaceError,
    },
    #[error("Worker Pool compensation failed: {0}")]
    Compensation(String),
}

/// The deep Worker Pool lifecycle module for one repository.
#[derive(Debug, Clone)]
pub struct WorkerPool {
    repository: Repository,
}

impl WorkerPool {
    pub(crate) fn new(repository: Repository) -> Self {
        Self { repository }
    }

    /// Reads a compatible Worker Pool snapshot without changing any file.
    pub fn snapshot(&self) -> WorkerPoolSnapshot {
        self.repository.read_worker_pool_snapshot()
    }

    /// Reserves the first idle Worker for `ticket` in pool order.
    pub fn reserve(&self, ticket: impl Into<String>) -> Result<Reservation, WorkerPoolError> {
        self.reserve_inner(None, ticket.into())
    }

    /// Reserves the named idle Worker for `ticket`.
    pub fn reserve_named(
        &self,
        worker: WorkerId,
        ticket: impl Into<String>,
    ) -> Result<Reservation, WorkerPoolError> {
        self.reserve_inner(Some(worker), ticket.into())
    }

    fn reserve_inner(
        &self,
        requested: Option<WorkerId>,
        ticket: String,
    ) -> Result<Reservation, WorkerPoolError> {
        let started_at = current_timestamp()?;
        let outcome = self.repository.state_store().reserve_worker(
            requested.as_ref(),
            ticket.clone(),
            started_at,
            ticket.to_lowercase(),
        )?;
        match outcome {
            crate::state::ReservationOutcome::Reserved {
                worker,
                agent_runtime,
                revision,
            } => Ok(Reservation {
                worker_id: worker,
                ticket,
                agent_runtime,
                repository: self.repository.clone(),
                worker_revision: revision,
            }),
            crate::state::ReservationOutcome::NoIdle { available } => {
                Err(WorkerPoolError::NoIdleWorkers { ticket, available })
            }
            crate::state::ReservationOutcome::WorkerNotInPool { worker } => {
                Err(WorkerPoolError::WorkerNotInPool { worker })
            }
            crate::state::ReservationOutcome::WorkerNotIdle { worker } => {
                Err(WorkerPoolError::WorkerNotIdle { worker })
            }
            crate::state::ReservationOutcome::InvalidAgentRuntime { value } => {
                Err(WorkerPoolError::InvalidAgentRuntime { value })
            }
        }
    }

    /// Grows the pool to `capacity`, provisioning stable Worker identities.
    ///
    /// A missing pool is first initialized with compatible metadata. Workspace
    /// commands run outside state locks; the final manifest update uses the
    /// loaded exact-byte revision, and newly provisioned Workers are
    /// compensated if another process wins the mutation.
    pub fn grow_to(&self, capacity: PoolCapacity) -> Result<PoolGrowth, WorkerPoolError> {
        let pool = self.repository.state_store().pool();
        let current = self.load_or_create(&pool)?;
        let current_size = usize::try_from(current.value.size)
            .map_err(|_| WorkerPoolError::InvalidSize(current.value.size))?;
        if capacity.as_usize() < current_size {
            return Err(WorkerPoolError::CannotShrink {
                current: current_size,
                requested: capacity.as_usize(),
            });
        }
        if capacity.as_usize() == current_size {
            return Ok(PoolGrowth {
                capacity,
                added_workers: Vec::new(),
            });
        }

        let mut known = current
            .value
            .workers
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let count = capacity.as_usize() - current_size;
        let mut added = Vec::with_capacity(count);
        for _ in 0..count {
            let worker = next_worker_id(&self.repository, &known)?;
            match self.repository.provision_worker_workspace(&worker) {
                Ok(_) => {
                    known.insert(worker.clone());
                    added.push(worker);
                }
                Err(source) => {
                    return match self.cleanup_workers(&added) {
                        Ok(()) => Err(WorkerPoolError::Provision { worker, source }),
                        Err(cleanup) => Err(WorkerPoolError::Compensation(format!(
                            "{source}; {cleanup}"
                        ))),
                    };
                }
            }
        }

        let mut next = current.value.clone();
        next.size = i64::try_from(capacity.as_usize())
            .map_err(|_| WorkerPoolError::InvalidSize(next.size))?;
        next.workers.extend(added.iter().cloned());
        let committed = pool.commit(
            Expected::Match(current.revision().clone()),
            StateChange::Replace(next),
        );
        match committed {
            Ok(CommitOutcome::Applied(_)) => Ok(PoolGrowth {
                capacity,
                added_workers: added,
            }),
            Ok(CommitOutcome::Conflict(_)) => match self.cleanup_workers(&added) {
                Ok(()) => Err(WorkerPoolError::Conflict),
                Err(cleanup) => Err(WorkerPoolError::Compensation(cleanup.to_string())),
            },
            Err(error) => match self.cleanup_workers(&added) {
                Ok(()) => Err(WorkerPoolError::State(error)),
                Err(cleanup) => Err(WorkerPoolError::Compensation(format!("{error}; {cleanup}"))),
            },
        }
    }

    fn cleanup_workers(&self, workers: &[WorkerId]) -> Result<(), WorkerPoolError> {
        let mut failures = Vec::new();
        for worker in workers {
            if let Err(error) = crate::workspace::deprovision(&self.repository, worker) {
                failures.push(format!("{worker}: {error}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(WorkerPoolError::Compensation(failures.join("; ")))
        }
    }

    fn load_or_create(
        &self,
        pool: &crate::PoolStateRepository,
    ) -> Result<crate::Versioned<PoolState>, WorkerPoolError> {
        match pool.load()? {
            Loaded::Present(versioned) => Ok(versioned),
            Loaded::Missing => {
                let empty = PoolState::new(
                    0,
                    discover_gh_repo(&self.repository)?,
                    Vec::new(),
                    current_timestamp()?,
                );
                match pool.commit(Expected::Missing, StateChange::Replace(empty))? {
                    CommitOutcome::Applied(Loaded::Present(versioned))
                    | CommitOutcome::Conflict(Loaded::Present(versioned)) => Ok(versioned),
                    CommitOutcome::Applied(Loaded::Missing)
                    | CommitOutcome::Conflict(Loaded::Missing) => Err(WorkerPoolError::Conflict),
                }
            }
        }
    }
}

fn next_worker_id(
    repository: &Repository,
    known: &BTreeSet<WorkerId>,
) -> Result<WorkerId, WorkerPoolError> {
    for _ in 0..32 {
        let mut bytes = [0_u8; 3];
        getrandom::fill(&mut bytes)
            .map_err(|error| WorkerPoolError::WorkerId(error.to_string()))?;
        let candidate = WorkerId::parse(format!(
            "worker-{:02x}{:02x}{:02x}",
            bytes[0], bytes[1], bytes[2]
        ))
        .map_err(|error| WorkerPoolError::WorkerId(error.to_string()))?;
        if !known.contains(&candidate) && !worker_claimed(repository, &candidate) {
            return Ok(candidate);
        }
    }
    Err(WorkerPoolError::WorkerId(
        "could not find an unused identifier after 32 attempts".to_owned(),
    ))
}

fn worker_claimed(repository: &Repository, worker: &WorkerId) -> bool {
    crate::workspace::worker_path(repository.root(), worker).exists()
        || repository
            .state_store()
            .worker(worker.clone())
            .load()
            .is_ok_and(|state| matches!(state, Loaded::Present(_)))
}

fn discover_gh_repo(repository: &Repository) -> Result<String, WorkerPoolError> {
    let output = Command::new("jj")
        .args(["git", "remote", "list"])
        .current_dir(repository.root())
        .output()
        .map_err(|error| WorkerPoolError::RepositoryDiscovery(error.to_string()))?;
    if !output.status.success() {
        return Err(WorkerPoolError::RepositoryDiscovery(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() >= 2 && fields[0] == "origin" {
            return Ok(remote_slug(fields[1]));
        }
    }
    Ok(String::new())
}

fn remote_slug(remote: &str) -> String {
    let remote = remote.trim_end_matches(".git");
    if let Some((_, path)) = remote.rsplit_once(':') {
        return path.to_owned();
    }
    let mut parts = remote.rsplitn(3, '/');
    match (parts.next(), parts.next()) {
        (Some(name), Some(owner)) => format!("{owner}/{name}"),
        _ => remote.to_owned(),
    }
}

fn current_timestamp() -> Result<WireTimestamp, WorkerPoolError> {
    let timestamp = Timestamp::now()
        .round(
            TimestampRound::new()
                .smallest(Unit::Second)
                .mode(RoundMode::Trunc),
        )
        .map_err(|error| WorkerPoolError::Timestamp(error.to_string()))?;
    Ok(WireTimestamp::new(timestamp.to_string()))
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
    use rustix::process::{test_kill_process, Pid};

    i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .is_some_and(|pid| test_kill_process(pid).is_ok())
}
#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    true
}
