//! The workspace-dispatch engine: repository discovery, Workspaces, the Worker
//! Pool, agent runtimes, run logs, tickets, and dispatch.
//!
//! Absorbed from the former `wsg-core` crate so `jjfx` ships as a single crate.
//! The CLI reaches it through [`crate::engine`].

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

mod atomic;
mod cost;
mod direct_dispatch;
mod dispatch_group;
mod dispatch_prompt;
mod eligibility;
mod foreground;
mod governor;
mod layout;
mod local_tickets;
mod opencode_api;
mod orchestration;
mod pool;
mod reviewer;
mod run_log;
mod runtime;
mod state;
mod ticket;
mod undo;
mod watchdog;
mod worker_actions;
mod workspace;

pub use cost::{PoolCost, PoolCostSnapshot, SpendCap, SpendCapMode, SpendDecision, SpendWindow};
pub use direct_dispatch::{
    DirectDispatch, DirectDispatchError, DirectDispatchExecution, DirectDispatchFailure,
    DirectDispatchFailurePhase, DirectDispatchOutcome, DirectDispatchRequest, DirectDispatchResult,
    DirectDispatchSuccess, DirectDispatchTarget, DirectDispatchWaiting, DispatchDependencyContext,
};
pub use dispatch_group::{
    DispatchGroup, DispatchGroupBuildOptions, DispatchGroupError, DispatchGroupEvent,
    DispatchGroupProgress, DispatchGroupStatusCounts, DispatchGroupTransition,
    DispatchIssueProgress, SubIssueStatus, UnknownSubIssueStatus,
};
pub use dispatch_prompt::{
    DeliveryContract, DispatchBudget, DispatchPromptBuilder, DispatchPromptContext,
    DispatchPromptError,
};
pub use eligibility::{DispatchExclusions, ExclusionsProvider, reconcile_occupancy};
pub use foreground::{FOREGROUND_WRAPPER_ENV, WRAPPER_SUBCOMMAND, run_wrapper};
pub use governor::ConcurrencyCap;
pub use layout::{
    DEFAULT_BIAS, PaneLayout, PanePlacement, PaneRole, SplitLocation, TerminalSettings,
};
pub use local_tickets::{
    CLAIMED_STATUS, LocalMarkdownQuery, LocalTicket, LocalTicketError, LocalTicketMalformation,
    LocalTicketUpdate, LocalTickets,
};
pub use opencode_api::{
    CommandOutput, ModelRef, NewSession, OpenCodeApi, OpenCodeError, OpenCodeRunner,
    PermissionDecision, PermissionRequest, ProcessOpenCodeRunner, Session, SessionOutcome,
};
pub use orchestration::{
    OrchestrationError, OrchestrationEvent, OrchestrationOptions, OrchestrationPreparation,
    OrchestrationRequest, OrchestrationRunner, OrchestrationStart, OrchestrationSummary,
};
pub use pool::{
    CapacityShortage, PersistedField, PoolCapacity, PoolCapacityError, PoolResize, PoolSnapshot,
    Reservation, SnapshotDiagnostic, SnapshotDiagnosticKind, WorkerPool, WorkerPoolError,
    WorkerPoolSnapshot, WorkerReference, WorkerSnapshot, WorkerStatus,
};
pub use reviewer::{
    CodeRabbitReviewer, ReviewCompleteness, ReviewFinding, ReviewLoop, ReviewLoopError,
    ReviewLoopSummary, ReviewOutcome, ReviewRequest, ReviewSeverity, ReviewStop, Reviewer,
    ReviewerError,
};
pub use run_log::{
    AgentSessionResolution, CollaborationEvent, CollaborationParticipant, FreshSessionReason,
    RunActivity, RunActivityKind, RunActivityStatus, RunConclusion, RunCost, RunLog, RunLogError,
    RunLogEvent, RunLogParseError, RunLogParser, RunResult, RunResultFallback, RunResultSource,
    RunUsage, resolve_agent_session, resolve_agent_session_for_runtime,
};
pub use runtime::{
    AgentModel, AgentRuntime, AgentRuntimeCapabilities, AgentRuntimeInvocation,
    AgentRuntimeProbeError, AgentRuntimeProfile, BackgroundRun, CompletedRun, RunOutcome,
    RunRequest, RunReset, RunSupervisor, RunSupervisorError,
};
pub use state::{
    CommitOutcome, DispatchGroupOptions, DispatchGroupState, DispatchGroupStateRepository,
    Expected, IdentifierError, InUseState, InUseStateRepository, Loaded, MutationKind,
    MutationRecord, MutationRecordRepository, PoolState, PoolStateRepository,
    REPOSITORY_WIDE_MUTATION, RunRecord, RunRecordConclusion, RunRecordRepository, RunRecordUsage,
    SessionState, SessionStateRepository, StateChange, StateError, StateRevision, StateStore,
    SubIssueState, TicketId, UndoState, Versioned, WaitingState, WaitingStateRepository, WireAgent,
    WireStatus, WireTimestamp, WorkerId, WorkerState, WorkerStateRepository,
};
pub use ticket::{
    AgentRuntimeQuery, Blocker, DependencyGraph, DiscoveredSubIssue, DiscoveryDiagnostic,
    ParentTicket, ReadyTicketFilter, ReadyTickets, RepositoryIdentity, Ticket, TicketDetails,
    TicketDiscovery, TicketDiscoveryError, TicketQuery, TicketQueryError, TicketQueryErrorKind,
    TicketQueryRequest, TicketStatus, TicketTitle, TicketValueError,
};
pub use undo::{MutationCheckpoint, MutationGuard, UndoError, UndoOutcome};
pub use watchdog::{RunHealth, RunProgress, StuckWindow};
pub use worker_actions::{
    AbsorbOutcome, AbsorbReport, DismissOutcome, FollowUpExecution, FollowUpOutcome,
    FollowUpSession, MountOutcome, OpenPullRequestOutcome, PermissionResolution, RebaseOutcome,
    ResetOutcome, RunMode, WorkerActionError, WorkerActions, WorkerLogs, WorkspaceRestoration,
    WorkspaceRestorationError, WorkspaceRestorationHandle,
};
pub use workspace::jj_lib_compat_notice;
pub use workspace::{
    AdHocWorkspace, AdHocWorkspaceError, AdHocWorkspacePhase, CleanDecision, WorkerWorkspace,
    WorkerWorkspaceError, WorkspaceAddOutcome, WorkspaceCleanPlan, WorkspaceEntry,
    WorkspaceHookStream, WorkspaceSnapshot, Workspaces, ad_hoc_workspace_path,
};

/// A Jujutsu repository discovered from a starting path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    root: PathBuf,
    colocate: bool,
}

impl Repository {
    /// Whether new Workspaces are colocated with Git (a Git worktree with a
    /// working `.git`). jjfx's `colocate` setting, on by default. A repository
    /// that is not itself colocated never colocates, whatever this says.
    pub fn colocate(&self) -> bool {
        self.colocate
    }

    /// Returns this repository with the Workspace colocation preference set.
    /// Keeping this a builder lets [`Repository::open`] callers (and their
    /// tests) stay on the default without threading the setting everywhere.
    pub fn with_colocate(mut self, colocate: bool) -> Self {
        self.colocate = colocate;
        self
    }

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
                return Ok(Self {
                    root,
                    colocate: true,
                });
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

    /// Opens the deep Direct Dispatch coordinator for this repository.
    pub fn direct_dispatch(&self) -> DirectDispatch {
        DirectDispatch::new(self.clone())
    }

    /// Opens persistent Dispatch Group orchestration for this repository.
    pub fn orchestration_runner(&self) -> OrchestrationRunner {
        OrchestrationRunner::new(self.clone())
    }

    /// Opens the deep Repository-owned Workspace module.
    pub fn workspaces(&self) -> Workspaces {
        Workspaces::new(self.clone())
    }

    /// Creates an Ad Hoc Workspace at the jjfx-compatible sibling path.
    pub fn create_ad_hoc_workspace(
        &self,
        requested_name: &str,
    ) -> Result<AdHocWorkspace, AdHocWorkspaceError> {
        self.create_ad_hoc_workspace_with_revision(requested_name, None)
    }

    /// Creates an Ad Hoc Workspace with an optional explicit parent revision.
    pub fn create_ad_hoc_workspace_with_revision(
        &self,
        requested_name: &str,
        revision: Option<&str>,
    ) -> Result<AdHocWorkspace, AdHocWorkspaceError> {
        self.create_ad_hoc_workspace_with_revision_and_progress(requested_name, revision, |_, _| {})
    }

    /// Creates an Ad Hoc Workspace while reporting lifecycle hook output.
    ///
    /// The callback runs synchronously from the caller's thread. Callers that
    /// must remain responsive should invoke this blocking operation from their
    /// own worker thread.
    pub fn create_ad_hoc_workspace_with_revision_and_progress<F>(
        &self,
        requested_name: &str,
        revision: Option<&str>,
        on_output: F,
    ) -> Result<AdHocWorkspace, AdHocWorkspaceError>
    where
        F: FnMut(WorkspaceHookStream, &str),
    {
        workspace::create_ad_hoc(self, requested_name, revision, on_output)
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

    /// Provisions a Worker Workspace while reporting lifecycle hook output.
    pub fn provision_worker_workspace_with_progress<F>(
        &self,
        worker_id: &WorkerId,
        on_output: F,
    ) -> Result<WorkerWorkspace, WorkerWorkspaceError>
    where
        F: FnMut(WorkspaceHookStream, &str),
    {
        workspace::provision_with_progress(self, worker_id, on_output)
    }

    pub(crate) fn prepare_worker_workspace_for_dispatch(
        &self,
        worker_id: &WorkerId,
        base_revisions: &[String],
    ) -> Result<workspace::PreparedWorkerWorkspace, WorkerWorkspaceError> {
        workspace::prepare_for_dispatch(self, worker_id, base_revisions)
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
