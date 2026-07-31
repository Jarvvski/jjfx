//! Shared Workspace Dispatch foundations for the `jjfx` and `wsg` binaries.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

mod pool;
mod run_log;
mod runtime;
mod state;
mod workspace;

pub use pool::{
    PersistedField, PoolCapacity, PoolCapacityError, PoolResize, PoolSnapshot, Reservation,
    SnapshotDiagnostic, SnapshotDiagnosticKind, WorkerPool, WorkerPoolError, WorkerPoolSnapshot,
    WorkerReference, WorkerSnapshot, WorkerStatus,
};
pub use run_log::{
    AgentSessionResolution, CollaborationEvent, CollaborationParticipant, FreshSessionReason,
    RunActivity, RunActivityKind, RunActivityStatus, RunConclusion, RunCost, RunLog, RunLogError,
    RunLogEvent, RunLogParseError, RunLogParser, RunResult, RunUsage, resolve_agent_session,
};
pub use runtime::{
    AgentRuntime, AgentRuntimeCapabilities, AgentRuntimeInvocation, AgentRuntimeProbeError,
    BackgroundRun, RunOutcome, RunRequest, RunReset, RunSupervisor, RunSupervisorError,
};
pub use state::{
    CommitOutcome, DispatchGroupOptions, DispatchGroupState, DispatchGroupStateRepository,
    Expected, IdentifierError, Loaded, PoolState, PoolStateRepository, StateChange, StateError,
    StateRevision, StateStore, SubIssueState, TicketId, Versioned, WireAgent, WireStatus,
    WireTimestamp, WorkerId, WorkerState, WorkerStateRepository,
};
pub use workspace::{AdHocWorkspace, AdHocWorkspaceError, WorkerWorkspace, WorkerWorkspaceError};

/// The migration capabilities currently exposed by a discovered repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationCapabilities {
    /// The shared Workspace Dispatch implementation can expose read-only pool state.
    ReadOnlyWorkerPool,
    /// The shared Workspace Dispatch implementation is not available yet.
    NotImplemented,
}

/// A Jujutsu repository discovered from a starting path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    root: PathBuf,
}

impl Repository {
    /// Discovers the nearest ancestor containing a `.jj` directory.
    pub fn open(start: impl AsRef<Path>) -> Result<Self, RepositoryError> {
        let start = start.as_ref().to_path_buf();
        let mut directory = start.clone();
        if !directory.is_absolute() {
            directory = std::env::current_dir()
                .map_err(|source| RepositoryError::PathResolution {
                    path: start.clone(),
                    source,
                })?
                .join(directory);
        }
        let mut directory =
            directory
                .canonicalize()
                .map_err(|source| RepositoryError::PathResolution {
                    path: start.clone(),
                    source,
                })?;

        loop {
            if directory.join(".jj").is_dir() {
                let root = default_workspace_root(&directory).map_err(|source| {
                    RepositoryError::PathResolution {
                        path: start.clone(),
                        source,
                    }
                })?;
                return Ok(Self { root });
            }
            if !directory.pop() {
                return Err(RepositoryError::NotFound { start });
            }
        }
    }

    /// Returns the canonical path of the repository workspace root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Opens the deep Worker Pool lifecycle module for this repository.
    pub fn worker_pool(&self) -> WorkerPool {
        WorkerPool::new(self.clone())
    }

    /// Creates an Ad Hoc Workspace at the compatible sibling path.
    pub fn create_ad_hoc_workspace(
        &self,
        requested_name: &str,
    ) -> Result<AdHocWorkspace, AdHocWorkspaceError> {
        workspace::create_ad_hoc(self, requested_name)
    }

    /// Removes an Ad Hoc Workspace and its optional known directory.
    pub fn remove_ad_hoc_workspace(
        &self,
        name: &str,
        known_path: Option<&Path>,
    ) -> Result<(), AdHocWorkspaceError> {
        workspace::remove_ad_hoc(self, name, known_path)
    }

    /// Provisions the Worker Workspace and idle Worker state for `worker_id`.
    ///
    /// This is a blocking operation. It creates the Go-compatible Workspace
    /// path, projects `.jj/ws-cache`, and writes an idle Worker document. If a
    /// later step fails, resources created by this call are compensated before
    /// the error is returned. Missing optional setup sources are valid; a
    /// failure copying a source that exists fails the operation.
    pub fn provision_worker_workspace(
        &self,
        worker_id: &WorkerId,
    ) -> Result<WorkerWorkspace, WorkerWorkspaceError> {
        workspace::provision(self, worker_id)
    }

    /// Reports which migration capabilities are available for this foundation.
    pub fn migration_capabilities(&self) -> MigrationCapabilities {
        MigrationCapabilities::ReadOnlyWorkerPool
    }
}

fn default_workspace_root(workspace_root: &Path) -> io::Result<PathBuf> {
    let repo_marker = workspace_root.join(".jj/repo");
    let metadata = match fs::metadata(&repo_marker) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(workspace_root.to_owned());
        }
        Err(error) => return Err(error),
    };
    if metadata.is_dir() {
        return Ok(workspace_root.to_owned());
    }

    let target = match fs::read_to_string(repo_marker) {
        Ok(target) => target.trim().to_owned(),
        Err(_) => return Ok(workspace_root.to_owned()),
    };
    if target.is_empty() {
        return Ok(workspace_root.to_owned());
    }
    let target = PathBuf::from(target);
    let target = if target.is_absolute() {
        target
    } else {
        workspace_root.join(".jj").join(target)
    };
    let resolved = target.canonicalize().unwrap_or(target);
    Ok(resolved
        .parent()
        .and_then(Path::parent)
        .unwrap_or(workspace_root)
        .to_owned())
}

/// Errors that can occur while discovering a Jujutsu repository.
#[derive(Debug, Error)]
pub enum RepositoryError {
    /// The starting path could not be resolved.
    #[error("cannot resolve repository path {path}")]
    PathResolution {
        /// The path supplied to [`Repository::open`].
        path: PathBuf,
        /// The operating-system error that prevented resolution.
        #[source]
        source: io::Error,
    },
    /// No repository marker was found at or above the starting path.
    #[error("not inside a Jujutsu repository: no .jj directory found above {start}")]
    NotFound {
        /// The path supplied to [`Repository::open`].
        start: PathBuf,
    },
}
