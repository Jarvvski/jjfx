//! Compatible Workspace Dispatch state repositories.
//!
//! This module is the only place that knows state paths, sidecar locks, JSON
//! codecs, and atomic replacement. Callers load typed values and commit them
//! with an opaque exact-byte revision; no caller code runs while a lock is held.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::marker::PhantomData;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::Repository;

const POOL_PATH: &str = ".jj/pool.json";
const POOL_DIRECTORY: &str = ".jj/pool";
const POOL_LOCK: &str = ".jj/pool/.dispatch.lock";

type Extensions = BTreeMap<String, Value>;

fn wire_agent_is_absent(agent: &Option<WireAgent>) -> bool {
    agent.as_ref().is_none_or(|agent| agent.as_str().is_empty())
}

fn serialize_branch_name<S>(branch: &Option<String>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match branch.as_deref() {
        Some(branch) if !branch.is_empty() => serializer.serialize_some(branch),
        Some(_) | None => serializer.serialize_none(),
    }
}

macro_rules! wire_string {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates a wire value without imposing lifecycle policy.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }
            /// Returns the persisted spelling.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

wire_string!(
    WireStatus,
    "An open, forward-compatible persisted status value."
);
wire_string!(
    WireAgent,
    "An open, forward-compatible Agent Runtime value."
);
wire_string!(WireTimestamp, "A timestamp preserved exactly as persisted.");

/// A validated Worker identifier that is safe as one filename component.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct WorkerId(String);

impl WorkerId {
    /// Validates a persisted Worker identifier.
    pub fn parse(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        validate_component(&value, "Worker")?;
        Ok(Self(value))
    }

    /// Returns the persisted identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for WorkerId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// A validated Ticket identifier that is safe in a Dispatch Group filename.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct TicketId(String);

impl TicketId {
    /// Validates a persisted Ticket identifier.
    pub fn parse(value: impl Into<String>) -> Result<Self, IdentifierError> {
        let value = value.into();
        validate_component(&value, "Ticket")?;
        Ok(Self(value))
    }

    /// Returns the persisted identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TicketId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for TicketId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

fn validate_component(value: &str, kind: &'static str) -> Result<(), IdentifierError> {
    let path = Path::new(value);
    if value.is_empty()
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err(IdentifierError {
            kind,
            value: value.to_owned(),
        });
    }
    Ok(())
}

/// An invalid compatibility identifier.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{kind} identifier {value:?} is not a safe filename component")]
pub struct IdentifierError {
    kind: &'static str,
    value: String,
}

/// The exact Go-compatible pool configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolState {
    /// Number of configured Workers.
    pub size: i64,
    /// GitHub repository identifier used by dispatch.
    pub gh_repo: String,
    /// Worker identifiers in stable pool order.
    pub workers: Vec<WorkerId>,
    /// Pool creation timestamp.
    pub created_at: WireTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Optional foreground execution default.
    pub foreground: Option<bool>,
    #[serde(default, skip_serializing_if = "wire_agent_is_absent")]
    /// Optional default Agent Runtime.
    pub agent: Option<WireAgent>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    /// Optional display aliases keyed by Worker.
    pub names: BTreeMap<WorkerId, String>,
    #[serde(flatten)]
    extra: Extensions,
}

impl PoolState {
    /// Creates a pool state with no optional settings or extensions.
    pub fn new(
        size: i64,
        gh_repo: impl Into<String>,
        workers: Vec<WorkerId>,
        created_at: WireTimestamp,
    ) -> Self {
        Self {
            size,
            gh_repo: gh_repo.into(),
            workers,
            created_at,
            foreground: None,
            agent: None,
            names: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }
}

/// The exact Go-compatible Worker state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerState {
    /// Persisted Worker Status.
    pub status: WireStatus,
    /// Agent Runtime selected for the current or latest Run.
    pub agent: Option<WireAgent>,
    /// Assigned Ticket, if any.
    pub ticket: Option<String>,
    /// Agent Runtime process identifier, if known.
    pub pid: Option<i64>,
    /// Run start timestamp.
    pub started_at: Option<WireTimestamp>,
    /// Run completion timestamp.
    pub completed_at: Option<WireTimestamp>,
    /// Structured Agent Runtime log path.
    pub log_file: Option<String>,
    #[serde(serialize_with = "serialize_branch_name")]
    /// Current bookmark name, or the dispatch placeholder before resolution.
    pub branch_name: Option<String>,
    /// Process or Agent Runtime exit code.
    pub exit_code: Option<i64>,
    /// Latest failure message.
    pub error: Option<String>,
    #[serde(flatten)]
    extra: Extensions,
}

impl WorkerState {
    /// Creates a Worker state with all optional wire fields explicitly absent.
    pub fn new(status: WireStatus) -> Self {
        Self {
            status,
            agent: None,
            ticket: None,
            pid: None,
            started_at: None,
            completed_at: None,
            log_file: None,
            branch_name: None,
            exit_code: None,
            error: None,
            extra: BTreeMap::new(),
        }
    }
}

/// One Sub-issue entry in a Dispatch Group.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubIssueState {
    /// Human-facing Ticket title.
    pub title: String,
    /// Persisted Sub-issue Status.
    pub status: WireStatus,
    /// Direct dependency Ticket identifiers.
    pub blocked_by: Vec<TicketId>,
    /// Assigned Worker, if dispatched.
    pub worker: Option<WorkerId>,
    /// Resulting bookmark, if known.
    pub branch: Option<String>,
    /// Dispatch timestamp.
    pub dispatched_at: Option<WireTimestamp>,
    /// Completion timestamp.
    pub completed_at: Option<WireTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Optional reason a Sub-issue was skipped.
    pub skip_reason: Option<String>,
    /// Number of completed retry attempts.
    pub retries: i64,
    #[serde(flatten)]
    extra: Extensions,
}

impl SubIssueState {
    /// Creates an unassigned Sub-issue state.
    pub fn new(title: impl Into<String>, status: WireStatus, blocked_by: Vec<TicketId>) -> Self {
        Self {
            title: title.into(),
            status,
            blocked_by,
            worker: None,
            branch: None,
            dispatched_at: None,
            completed_at: None,
            skip_reason: None,
            retries: 0,
            extra: BTreeMap::new(),
        }
    }
}

/// Provider options persisted with a Dispatch Group.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchGroupOptions {
    #[serde(default, skip_serializing_if = "wire_agent_is_absent")]
    /// Optional Agent Runtime override.
    pub agent: Option<WireAgent>,
    /// Optional model spelling, represented by an empty string when unset.
    pub model: String,
    #[serde(flatten)]
    extra: Extensions,
}

impl DispatchGroupOptions {
    /// Creates options without an Agent Runtime override.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            agent: None,
            model: model.into(),
            extra: BTreeMap::new(),
        }
    }
}

/// The exact Go-compatible Dispatch Group state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchGroupState {
    /// Parent Ticket that owns this Dispatch Group.
    pub parent: TicketId,
    /// Dispatch Group creation timestamp.
    pub created_at: WireTimestamp,
    /// GitHub repository identifier used by dispatch.
    pub gh_repo: String,
    /// Sub-issue state keyed by Ticket identifier.
    pub sub_issues: BTreeMap<TicketId, SubIssueState>,
    /// Agent Runtime and model options.
    pub opts: DispatchGroupOptions,
    #[serde(flatten)]
    extra: Extensions,
}

impl DispatchGroupState {
    /// Creates an empty Dispatch Group.
    pub fn new(
        parent: TicketId,
        created_at: WireTimestamp,
        gh_repo: impl Into<String>,
        opts: DispatchGroupOptions,
    ) -> Self {
        Self {
            parent,
            created_at,
            gh_repo: gh_repo.into(),
            sub_issues: BTreeMap::new(),
            opts,
            extra: BTreeMap::new(),
        }
    }
}

/// A loaded state document, including absence as data rather than an error.
#[derive(Debug, Clone, PartialEq)]
pub enum Loaded<T> {
    /// The state file does not exist.
    Missing,
    /// A validated state value and its exact-byte revision.
    Present(Versioned<T>),
}

/// A typed value paired with its opaque exact-byte revision.
#[derive(Debug, Clone, PartialEq)]
pub struct Versioned<T> {
    /// Loaded typed state.
    pub value: T,
    revision: StateRevision<T>,
}

impl<T> Versioned<T> {
    /// Returns the revision required for a compare-and-swap commit.
    pub fn revision(&self) -> &StateRevision<T> {
        &self.revision
    }
    /// Separates the typed value from its revision.
    pub fn into_parts(self) -> (T, StateRevision<T>) {
        (self.value, self.revision)
    }
}

/// An opaque exact-byte state revision.
pub struct StateRevision<T> {
    bytes: Vec<u8>,
    marker: PhantomData<fn() -> T>,
}
impl<T> Clone for StateRevision<T> {
    fn clone(&self) -> Self {
        Self {
            bytes: self.bytes.clone(),
            marker: PhantomData,
        }
    }
}
impl<T> PartialEq for StateRevision<T> {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}
impl<T> Eq for StateRevision<T> {}
impl<T> fmt::Debug for StateRevision<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StateRevision(..)")
    }
}

/// The state a commit expects to find after acquiring its lock.
#[derive(Debug, Clone, PartialEq)]
pub enum Expected<T> {
    /// Commit only if the target is absent.
    Missing,
    /// Commit only if the exact target bytes still match this revision.
    Match(StateRevision<T>),
}
/// A typed state change.
#[derive(Debug, Clone, PartialEq)]
pub enum StateChange<T> {
    /// Atomically replace the target with this typed state.
    Replace(T),
    /// Remove the target while retaining its stable sidecar lock.
    Remove,
}
/// The result of a commit attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum CommitOutcome<T> {
    /// The requested change was committed.
    Applied(Loaded<T>),
    /// The expected state did not match the state reloaded under lock.
    Conflict(Loaded<T>),
}

/// A contextual state repository failure.
#[derive(Debug, Error)]
#[error("cannot {operation} {subject}: {detail}")]
pub struct StateError {
    operation: &'static str,
    subject: String,
    detail: String,
}
impl StateError {
    fn new(operation: &'static str, subject: impl Into<String>, error: impl fmt::Display) -> Self {
        Self {
            operation,
            subject: subject.into(),
            detail: error.to_string(),
        }
    }
}

/// Repository-scoped access to all persisted Workspace Dispatch state.
#[derive(Debug, Clone)]
pub struct StateStore {
    root: PathBuf,
}

/// Result of the repository-owned atomic Worker Reservation transition.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReservationOutcome {
    Reserved {
        worker: WorkerId,
        agent_runtime: crate::AgentRuntime,
        revision: StateRevision<WorkerState>,
    },
    NoIdle {
        available: usize,
    },
    WorkerNotInPool {
        worker: WorkerId,
    },
    WorkerNotIdle {
        worker: WorkerId,
    },
    InvalidAgentRuntime {
        value: String,
    },
}

/// Pool state repository.
#[derive(Debug, Clone)]
pub struct PoolStateRepository {
    root: PathBuf,
}
/// Worker state repository.
#[derive(Debug, Clone)]
pub struct WorkerStateRepository {
    root: PathBuf,
    worker: WorkerId,
}
/// Dispatch Group state repository.
#[derive(Debug, Clone)]
pub struct DispatchGroupStateRepository {
    root: PathBuf,
    parent: TicketId,
}

impl Repository {
    /// Opens the compatible state repository facade.
    pub fn state_store(&self) -> StateStore {
        StateStore {
            root: self.root().to_owned(),
        }
    }
}

impl StateStore {
    /// Opens the Worker Pool state repository.
    pub fn pool(&self) -> PoolStateRepository {
        PoolStateRepository {
            root: self.root.clone(),
        }
    }
    /// Opens one Worker state repository.
    pub fn worker(&self, worker: WorkerId) -> WorkerStateRepository {
        WorkerStateRepository {
            root: self.root.clone(),
            worker,
        }
    }
    /// Opens one Dispatch Group state repository.
    pub fn dispatch_group(&self, parent: TicketId) -> DispatchGroupStateRepository {
        DispatchGroupStateRepository {
            root: self.root.clone(),
            parent,
        }
    }

    /// Atomically reserves an idle Worker while holding the compatible pool
    /// and Worker sidecar locks. This repository-owned transition reloads
    /// membership and Worker state under lock so lifecycle callers never
    /// make a claim from a stale snapshot.
    pub(crate) fn reserve_worker(
        &self,
        requested: Option<&WorkerId>,
        ticket: String,
        started_at: WireTimestamp,
        branch_name: String,
    ) -> Result<ReservationOutcome, StateError> {
        let pool_subject = "Worker Pool";
        with_locks(&[self.root.join(POOL_LOCK)], pool_subject, || {
            let pool = match load_state(&self.root.join(POOL_PATH), pool_subject, &validate_pool)? {
                Loaded::Present(versioned) => versioned.value,
                Loaded::Missing => {
                    return Err(StateError::new("reserve", pool_subject, "state is missing"));
                }
            };
            let worker_locks = pool
                .workers
                .iter()
                .map(|worker| {
                    self.root
                        .join(POOL_DIRECTORY)
                        .join(format!("{worker}.json.lock"))
                })
                .collect::<Vec<_>>();
            with_locks(&worker_locks, pool_subject, || {
                let pool =
                    match load_state(&self.root.join(POOL_PATH), pool_subject, &validate_pool)? {
                        Loaded::Present(versioned) => versioned.value,
                        Loaded::Missing => {
                            return Err(StateError::new(
                                "reserve",
                                pool_subject,
                                "state is missing",
                            ));
                        }
                    };
                if let Some(requested) = requested
                    && !pool.workers.iter().any(|worker| worker == requested)
                {
                    return Ok(ReservationOutcome::WorkerNotInPool {
                        worker: requested.clone(),
                    });
                }
                let agent_runtime = match crate::AgentRuntime::from_configured(pool.agent.as_ref())
                {
                    Ok(agent_runtime) => agent_runtime,
                    Err(value) => {
                        return Ok(ReservationOutcome::InvalidAgentRuntime { value });
                    }
                };

                let candidates = requested.into_iter().cloned().chain(
                    requested
                        .is_none()
                        .then(|| pool.workers.clone())
                        .into_iter()
                        .flatten(),
                );
                let mut available = 0;
                let mut selected = None;
                for worker in candidates {
                    let path = self
                        .root
                        .join(POOL_DIRECTORY)
                        .join(format!("{worker}.json"));
                    let state =
                        match load_state(&path, &format!("Worker {worker}"), &validate_worker)? {
                            Loaded::Present(versioned) => versioned.value,
                            Loaded::Missing => {
                                return Err(StateError::new(
                                    "reserve",
                                    format!("Worker {worker}"),
                                    "state is missing",
                                ));
                            }
                        };
                    if state.status.as_str() == "idle" {
                        available += 1;
                        if selected.is_none() {
                            selected = Some((worker, state));
                        }
                    } else if requested.is_some() {
                        return Ok(ReservationOutcome::WorkerNotIdle { worker });
                    }
                }

                let Some((worker, mut state)) = selected else {
                    return Ok(ReservationOutcome::NoIdle { available });
                };
                state.status = WireStatus::new("busy");
                state.agent = Some(WireAgent::new(agent_runtime.as_str()));
                state.ticket = Some(ticket);
                state.started_at = Some(started_at);
                state.log_file = Some(
                    self.root
                        .join(POOL_DIRECTORY)
                        .join(format!("{worker}.log"))
                        .to_string_lossy()
                        .into_owned(),
                );
                state.branch_name = Some(branch_name);
                state.completed_at = None;
                state.pid = None;
                state.exit_code = None;
                state.error = None;
                let path = self
                    .root
                    .join(POOL_DIRECTORY)
                    .join(format!("{worker}.json"));
                write_atomic(&path, &state, &format!("Worker {worker}"))?;
                let revision =
                    match load_state(&path, &format!("Worker {worker}"), &validate_worker)? {
                        Loaded::Present(versioned) => versioned.revision().clone(),
                        Loaded::Missing => {
                            return Err(StateError::new(
                                "reserve",
                                format!("Worker {worker}"),
                                "state disappeared after reservation",
                            ));
                        }
                    };
                Ok(ReservationOutcome::Reserved {
                    worker,
                    agent_runtime,
                    revision,
                })
            })
        })
    }
}

macro_rules! repository_methods {
    ($repository:ident, $state:ty, $path:expr, $locks:expr, $subject:expr, $validate:expr) => {
        impl $repository {
            /// Loads compatible state without taking a mutation lock.
            pub fn load(&self) -> Result<Loaded<$state>, StateError> {
                load_state(&$path(self), &$subject(self), &$validate)
            }
            /// Commits a typed compare-and-swap mutation under compatible locks.
            pub fn commit(
                &self,
                expected: Expected<$state>,
                change: StateChange<$state>,
            ) -> Result<CommitOutcome<$state>, StateError> {
                commit_state(
                    &$path(self),
                    &$locks(self),
                    &$subject(self),
                    expected,
                    change,
                    $validate,
                )
            }
        }
    };
}

repository_methods!(
    PoolStateRepository,
    PoolState,
    |s: &PoolStateRepository| s.root.join(POOL_PATH),
    |s: &PoolStateRepository| vec![s.root.join(POOL_LOCK)],
    |_s: &PoolStateRepository| "Worker Pool".to_owned(),
    validate_pool
);
repository_methods!(
    WorkerStateRepository,
    WorkerState,
    |s: &WorkerStateRepository| s
        .root
        .join(POOL_DIRECTORY)
        .join(format!("{}.json", s.worker)),
    |s: &WorkerStateRepository| vec![
        s.root
            .join(POOL_DIRECTORY)
            .join(format!("{}.json.lock", s.worker))
    ],
    |s: &WorkerStateRepository| format!("Worker {}", s.worker),
    validate_worker
);

impl WorkerStateRepository {
    /// Removes detached Worker state and its log together under the Worker lock.
    ///
    /// When `expected` is present, a newer Worker state is left untouched and
    /// reported as a conflict. With no expected revision the operation is
    /// idempotent and removes whichever detached state remains.
    pub(crate) fn remove_detached(
        &self,
        expected: Option<StateRevision<WorkerState>>,
    ) -> Result<bool, StateError> {
        let path = self
            .root
            .join(POOL_DIRECTORY)
            .join(format!("{}.json", self.worker));
        let subject = format!("Worker {}", self.worker);
        with_locks(&[sidecar(&path)], &subject, || {
            let current = load_state(&path, &subject, &validate_worker)?;
            if let Some(expected) = expected
                && !matches!(
                    &current,
                    Loaded::Present(versioned) if expected.bytes == versioned.revision.bytes
                )
            {
                return Ok(false);
            }
            let log = self
                .root
                .join(POOL_DIRECTORY)
                .join(format!("{}.log", self.worker));
            for target in [&log, &path] {
                match fs::remove_file(target) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(StateError::new("remove", &subject, error)),
                }
            }
            Ok(true)
        })
    }
}

impl DispatchGroupStateRepository {
    fn path(&self) -> PathBuf {
        self.root.join(POOL_DIRECTORY).join(format!(
            "dispatch-{}.json",
            self.parent.as_str().to_lowercase()
        ))
    }

    /// Loads the Dispatch Group without taking a mutation lock.
    pub fn load(&self) -> Result<Loaded<DispatchGroupState>, StateError> {
        let subject = format!("Dispatch Group {}", self.parent);
        load_state(&self.path(), &subject, &|group| {
            validate_dispatch_group_for(&self.parent, group)
        })
    }

    /// Commits a typed compare-and-swap mutation under Pool and group locks.
    pub fn commit(
        &self,
        expected: Expected<DispatchGroupState>,
        change: StateChange<DispatchGroupState>,
    ) -> Result<CommitOutcome<DispatchGroupState>, StateError> {
        let path = self.path();
        let subject = format!("Dispatch Group {}", self.parent);
        commit_state(
            &path,
            &[self.root.join(POOL_LOCK), sidecar(&path)],
            &subject,
            expected,
            change,
            |group| validate_dispatch_group_for(&self.parent, group),
        )
    }
}

fn sidecar(path: &Path) -> PathBuf {
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(".lock");
    PathBuf::from(sidecar)
}

fn load_state<T: for<'de> Deserialize<'de>>(
    path: &Path,
    subject: &str,
    validate: &impl Fn(&T) -> Result<(), String>,
) -> Result<Loaded<T>, StateError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Loaded::Missing),
        Err(error) => return Err(StateError::new("read", subject, error)),
    };
    decode(bytes, subject, validate)
}

fn decode<T: for<'de> Deserialize<'de>>(
    bytes: Vec<u8>,
    subject: &str,
    validate: &impl Fn(&T) -> Result<(), String>,
) -> Result<Loaded<T>, StateError> {
    let value: T = serde_json::from_slice(&bytes)
        .map_err(|error| StateError::new("decode", subject, error))?;
    validate(&value).map_err(|error| StateError::new("validate", subject, error))?;
    Ok(Loaded::Present(Versioned {
        value,
        revision: StateRevision {
            bytes,
            marker: PhantomData,
        },
    }))
}

fn commit_state<T: Serialize + for<'de> Deserialize<'de>>(
    path: &Path,
    locks: &[PathBuf],
    subject: &str,
    expected: Expected<T>,
    change: StateChange<T>,
    validate: impl Fn(&T) -> Result<(), String>,
) -> Result<CommitOutcome<T>, StateError> {
    with_locks(locks, subject, || {
        let current = load_state(path, subject, &validate)?;
        let matches = match (&expected, &current) {
            (Expected::Missing, Loaded::Missing) => true,
            (Expected::Match(expected), Loaded::Present(current)) => {
                expected.bytes == current.revision.bytes
            }
            _ => false,
        };
        if !matches {
            return Ok(CommitOutcome::Conflict(current));
        }
        match change {
            StateChange::Replace(value) => {
                validate(&value)
                    .map_err(|error| StateError::new("validate replacement for", subject, error))?;
                write_atomic(path, &value, subject)?;
            }
            StateChange::Remove => match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(StateError::new("remove", subject, error)),
            },
        }
        load_state(path, subject, &validate).map(CommitOutcome::Applied)
    })
}

fn with_locks<T>(
    paths: &[PathBuf],
    subject: &str,
    operation: impl FnOnce() -> Result<T, StateError>,
) -> Result<T, StateError> {
    let mut paths = paths.to_vec();
    paths.sort();
    paths.dedup();
    let mut files = Vec::with_capacity(paths.len());
    for path in &paths {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| StateError::new("create lock directory for", subject, error))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| StateError::new("open lock for", subject, error))?;
        flock(&file, FlockOperation::LockExclusive)
            .map_err(|error| StateError::new("lock", subject, error))?;
        files.push(file);
    }
    operation()
}

fn write_atomic<T: Serialize>(path: &Path, value: &T, subject: &str) -> Result<(), StateError> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| StateError::new("serialize", subject, error))?;
    bytes.push(b'\n');
    let parent = path
        .parent()
        .ok_or_else(|| StateError::new("replace", subject, "state path has no parent"))?;
    fs::create_dir_all(parent)
        .map_err(|error| StateError::new("create state directory for", subject, error))?;
    let mut temporary = NamedTempFile::new_in(parent)
        .map_err(|error| StateError::new("create temporary file for", subject, error))?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o644))
        .map_err(|error| StateError::new("set temporary permissions for", subject, error))?;
    temporary
        .write_all(&bytes)
        .map_err(|error| StateError::new("write", subject, error))?;
    temporary
        .flush()
        .map_err(|error| StateError::new("flush", subject, error))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| StateError::new("sync", subject, error))?;
    temporary
        .into_temp_path()
        .persist(path)
        .map_err(|error| StateError::new("replace", subject, error.error))?;
    Ok(())
}

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn validate_pool(pool: &PoolState) -> Result<(), String> {
    if pool.size < 0 {
        return Err("pool size must not be negative".to_owned());
    }
    if pool.size != i64::try_from(pool.workers.len()).map_err(|error| error.to_string())? {
        return Err("pool size does not match Worker count".to_owned());
    }
    let unique: BTreeSet<_> = pool.workers.iter().collect();
    if unique.len() != pool.workers.len() {
        return Err("pool contains duplicate Workers".to_owned());
    }
    Ok(())
}
fn validate_worker(worker: &WorkerState) -> Result<(), String> {
    if worker.status.as_str().is_empty() {
        return Err("Worker status must not be empty".to_owned());
    }
    Ok(())
}
fn validate_dispatch_group_for(
    parent: &TicketId,
    group: &DispatchGroupState,
) -> Result<(), String> {
    if &group.parent != parent {
        return Err("Dispatch Group parent does not match repository identity".to_owned());
    }
    if group.sub_issues.values().any(|issue| issue.retries < 0) {
        return Err("Sub-issue retries must not be negative".to_owned());
    }
    Ok(())
}
