//! Frontend-neutral actions over one Worker.

use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread::{self, JoinHandle};

use serde::Deserialize;
use thiserror::Error;

use crate::engine::layout::{PaneLayout, PaneRole};
use crate::engine::runtime::pi_interactive_command;
use crate::engine::{
    AgentModel, AgentRuntime, AgentRuntimeCommandError, AgentRuntimeInvocation,
    AgentRuntimePreflightError, AgentRuntimeProbeError, AgentSessionResolution, BackgroundRun,
    CompletedRun, DispatchExclusions, PermissionDecision, PermissionRequest, Repository,
    Reservation, ReviewFinding, ReviewRequest, RunLog, RunSupervisor, RunSupervisorError, WorkerId,
    WorkerPoolError, WorkerStatus, resolve_agent_session_for_runtime,
};

/// Whether a Worker action runs attached to the caller or in the background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Wait for the Agent Runtime before returning.
    Foreground,
    /// Return after the process identifier has been persisted.
    Background,
}

/// The execution side of a launched Follow-up.
#[derive(Debug)]
pub enum FollowUpExecution {
    /// The foreground Run has already completed and finalized its Worker.
    Foreground(CompletedRun),
    /// The background Run must be waited on to reap and finalize it.
    Background(Box<BackgroundRun>),
    /// The prompt was delivered to the Run's opencode session through the
    /// daemon API, so there is no client process to reap. The Worker's liveness
    /// follows the session (ticket 37): reconcile finishes the Run on the
    /// session's outcome rather than on a process.
    Session(FollowUpSession),
}

/// A Follow-up delivered to an opencode Run's session without a client process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowUpSession {
    session_id: String,
}

impl FollowUpSession {
    pub(crate) fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
        }
    }

    /// The session the prompt was delivered to.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

/// The typed result of launching one Follow-up Run.
#[derive(Debug)]
pub struct FollowUpOutcome {
    worker: WorkerId,
    runtime: AgentRuntime,
    session: AgentSessionResolution,
    execution: FollowUpExecution,
}

impl FollowUpOutcome {
    /// Returns the Worker that owns the launched Run.
    pub const fn worker(&self) -> &WorkerId {
        &self.worker
    }

    /// Returns the Agent Runtime selected from compatible Worker or Pool state.
    pub const fn runtime(&self) -> AgentRuntime {
        self.runtime
    }

    /// Reports whether the previous Agent Session was resumed or why it was not.
    pub const fn session(&self) -> &AgentSessionResolution {
        &self.session
    }

    /// Returns the foreground completion or background Run handle.
    pub const fn execution(&self) -> &FollowUpExecution {
        &self.execution
    }

    /// Consumes the outcome and returns its execution value.
    pub fn into_execution(self) -> FollowUpExecution {
        self.execution
    }
}

/// The typed outcome of abandoning a Run and restoring its Workspace.
#[derive(Debug)]
pub struct ResetOutcome {
    run: crate::engine::RunReset,
    restoration: WorkspaceRestoration,
}

impl ResetOutcome {
    /// Returns how Run cleanup changed the Worker lifecycle.
    pub const fn run(&self) -> crate::engine::RunReset {
        self.run
    }

    /// Returns the asynchronous Workspace restoration state.
    pub const fn restoration(&self) -> &WorkspaceRestoration {
        &self.restoration
    }

    /// Consumes the outcome and returns the restoration state or handle.
    pub fn into_restoration(self) -> WorkspaceRestoration {
        self.restoration
    }
}

/// The compatibility outcome of dismissing one Worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DismissOutcome {
    /// An idle Worker was removed from the Pool.
    Removed { capacity: usize },
    /// A terminal Worker was cleared in place and kept its Workspace.
    Reset,
}

/// Workspace restoration scheduled after a Reset releases capacity.
#[derive(Debug)]
pub enum WorkspaceRestoration {
    /// The Worker Workspace directory no longer exists, so no command is needed.
    SkippedMissingWorkspace,
    /// Restoration is running independently of the now-idle Worker state.
    Pending(WorkspaceRestorationHandle),
}

/// An observable asynchronous Workspace restoration.
#[must_use = "Workspace restoration failures are observed by waiting on the handle"]
#[derive(Debug)]
pub struct WorkspaceRestorationHandle {
    worker: WorkerId,
    join: JoinHandle<Result<(), WorkspaceRestorationError>>,
}

impl WorkspaceRestorationHandle {
    /// Waits for `jj restore` and `jj new main` to complete.
    pub fn wait(self) -> Result<(), WorkspaceRestorationError> {
        self.join
            .join()
            .map_err(|_| WorkspaceRestorationError::ThreadPanicked {
                worker: self.worker,
            })?
    }
}

/// Failure while asynchronously restoring a Worker Workspace.
#[derive(Debug, Error)]
pub enum WorkspaceRestorationError {
    /// A restoration command could not start or returned a failure.
    #[error("cannot {operation} for Worker {worker}: {detail}")]
    Command {
        worker: WorkerId,
        operation: &'static str,
        detail: String,
    },
    /// The restoration worker thread panicked.
    #[error("Workspace restoration thread panicked for Worker {worker}")]
    ThreadPanicked { worker: WorkerId },
}

/// The result of rebasing and pushing one Worker bookmark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebaseOutcome {
    branch: String,
}

impl RebaseOutcome {
    /// Returns the rebased bookmark.
    pub fn branch(&self) -> &str {
        &self.branch
    }
}

/// What `jj absorb` did with one Worker's working-copy edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbsorbReport {
    revisions: usize,
    remaining: bool,
}

impl AbsorbReport {
    /// Returns how many ancestor commits took absorbed hunks.
    pub const fn revisions(&self) -> usize {
        self.revisions
    }

    /// Reports whether hunks stayed in the source revision, because no ancestor
    /// unambiguously owned the lines they changed.
    pub const fn has_remaining_changes(&self) -> bool {
        self.remaining
    }

    /// Reports whether absorb moved nothing at all.
    pub const fn is_empty(&self) -> bool {
        self.revisions == 0
    }
}

/// The result of folding a Worker's working-copy edits into their base commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbsorbOutcome {
    branch: String,
    report: AbsorbReport,
}

impl AbsorbOutcome {
    /// Returns the bookmark whose rewritten history was published.
    pub fn branch(&self) -> &str {
        &self.branch
    }

    /// Returns what absorb moved.
    pub const fn report(&self) -> AbsorbReport {
        self.report
    }
}

/// The result of opening one Worker's pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPullRequestOutcome {
    branch: String,
}

impl OpenPullRequestOutcome {
    /// Returns the bookmark used to find the pull request.
    pub fn branch(&self) -> &str {
        &self.branch
    }
}

/// A typed reference to one Worker's structured provider log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerLogs {
    path: std::path::PathBuf,
    runtime: AgentRuntime,
}

impl WorkerLogs {
    /// Returns the compatible provider log path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the provider required to interpret the log.
    pub const fn runtime(&self) -> AgentRuntime {
        self.runtime
    }

    /// Opens the log through the provider-neutral parser facade.
    pub fn open(&self) -> RunLog {
        RunLog::new(&self.path, self.runtime)
    }
}

/// The result of mounting a Worker Workspace into kitty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountOutcome {
    runtime: AgentRuntime,
    session: AgentSessionResolution,
    tab_id: String,
}

impl MountOutcome {
    /// Returns the interactive Agent Runtime opened in the tab.
    pub const fn runtime(&self) -> AgentRuntime {
        self.runtime
    }

    /// Reports whether the interactive Agent Session can resume.
    pub const fn session(&self) -> &AgentSessionResolution {
        &self.session
    }

    /// Returns kitty's new tab window identifier.
    pub fn tab_id(&self) -> &str {
        &self.tab_id
    }
}

/// The result of answering a Worker's pending opencode permission request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionResolution {
    worker: WorkerId,
    request: PermissionRequest,
    decision: PermissionDecision,
}

impl PermissionResolution {
    /// Returns the Worker whose pending request was answered.
    pub const fn worker(&self) -> &WorkerId {
        &self.worker
    }

    /// Returns the request that was answered.
    pub const fn request(&self) -> &PermissionRequest {
        &self.request
    }

    /// Returns the decision the request was answered with.
    pub const fn decision(&self) -> PermissionDecision {
        self.decision
    }
}

/// The deep frontend-neutral interface for operational Worker actions.
#[derive(Debug, Clone)]
pub struct WorkerActions {
    repository: Repository,
    supervisor: RunSupervisor,
    commands: SystemCommands,
    model: Option<AgentModel>,
    first_pane_command: Vec<String>,
    layout: PaneLayout,
    exclusions: DispatchExclusions,
    review_findings: Vec<ReviewFinding>,
}

impl WorkerActions {
    /// Opens Worker actions for one Repository.
    pub fn new(repository: Repository) -> Self {
        Self {
            repository,
            supervisor: RunSupervisor::new(),
            commands: SystemCommands,
            model: None,
            first_pane_command: Vec::new(),
            layout: PaneLayout::builtin(),
            exclusions: DispatchExclusions::default(),
            review_findings: Vec::new(),
        }
    }

    /// Supplies an AI reviewer's normalised findings to inline into the next
    /// review prompt. Empty (the default) leaves the prompt as it was.
    pub fn with_review_findings(mut self, findings: Vec<ReviewFinding>) -> Self {
        self.review_findings = findings;
        self
    }

    /// Excludes the given Worker Workspaces from Follow-up selection.
    ///
    /// A Worker whose Workspace is open in a terminal is not a valid target for
    /// `send`; `review` deliberately ignores this so a waiting Worker can still
    /// be told to address its pull request.
    pub fn with_exclusions(mut self, exclusions: DispatchExclusions) -> Self {
        self.exclusions = exclusions;
        self
    }

    /// Supplies the provider-aware model profile used by Worker actions.
    ///
    /// Pi requires both provider and model values. Other runtimes retain their
    /// existing provider-managed defaults when no profile is supplied.
    pub fn with_model(mut self, model: impl Into<AgentModel>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Supplies the resolved command (program + args) for a mounted tab's first
    /// pane. Empty (the default) opens that pane as a plain shell.
    pub fn with_first_pane_command(mut self, command: Vec<String>) -> Self {
        self.first_pane_command = command;
        self
    }

    /// Supplies the pane layout for mounted tabs. Defaults to the built-in
    /// layout jjfx has always built.
    pub fn with_layout(mut self, layout: PaneLayout) -> Self {
        self.layout = layout;
        self
    }

    /// Starts a new Run carrying a user Follow-up prompt.
    ///
    /// A compatible provider Session is resumed when the prior log identifies
    /// one. Otherwise the Run starts fresh and receives the Follow-up system
    /// prompt. The prior terminal or idle Worker state is restored if launch
    /// fails before the new Run becomes owned by its waiter.
    pub fn send(
        &self,
        worker: &WorkerId,
        prompt: impl Into<String>,
        mode: RunMode,
    ) -> Result<FollowUpOutcome, WorkerActionError> {
        if self.exclusions.contains(worker) {
            return Err(WorkerPoolError::WorkerNotEligible {
                worker: worker.clone(),
            }
            .into());
        }
        let prompt = prompt.into();
        self.follow_up(
            worker,
            &prompt,
            Some(send_system_prompt(&repository_slug(&self.repository))),
            mode,
        )
    }

    /// Send a prompt to the first Worker that is idle when the reservation is attempted.
    pub fn send_any(
        &self,
        prompt: impl Into<String>,
        mode: RunMode,
    ) -> Result<FollowUpOutcome, WorkerActionError> {
        let prompt = prompt.into();
        let snapshot = self.repository.worker_pool().snapshot();
        let candidates = snapshot
            .workers()
            .iter()
            .filter(|worker| worker.status() == WorkerStatus::Idle)
            .map(|worker| worker.worker_id().clone())
            .filter(|worker| !self.exclusions.contains(worker))
            .collect::<Vec<_>>();
        let system_prompt = send_system_prompt(&repository_slug(&self.repository));

        for worker in candidates {
            match self.follow_up(&worker, &prompt, Some(system_prompt.clone()), mode) {
                Ok(outcome) => return Ok(outcome),
                Err(WorkerActionError::WorkerPool(WorkerPoolError::WorkerNotIdle { .. }))
                | Err(WorkerActionError::WorkerNotFound { .. }) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(WorkerActionError::NoIdleWorker)
    }

    /// Returns the Worker's structured Run log without choosing a rendering.
    pub fn logs(&self, worker: &WorkerId) -> Result<WorkerLogs, WorkerActionError> {
        let snapshot = self.repository.worker_pool().snapshot();
        let state =
            snapshot
                .worker(worker.as_str())
                .ok_or_else(|| WorkerActionError::WorkerNotFound {
                    worker: worker.clone(),
                })?;
        let path = state
            .log_file()
            .filter(|path| !path.is_empty())
            .map(std::path::PathBuf::from)
            .ok_or_else(|| WorkerActionError::MissingLog {
                worker: worker.clone(),
            })?;
        if !path.is_file() {
            return Err(WorkerActionError::MissingLog {
                worker: worker.clone(),
            });
        }
        let runtime = state
            .agent_runtime()
            .ok_or_else(|| WorkerActionError::MissingRuntime {
                worker: worker.clone(),
            })?;
        Ok(WorkerLogs { path, runtime })
    }

    /// Rebases the Worker's bookmark onto `main` and pushes it.
    pub fn rebase(&self, worker: &WorkerId) -> Result<RebaseOutcome, WorkerActionError> {
        let (branch, workspace) = self.branch_and_workspace(worker)?;
        // A rebase rewrites the working copy, so serialise it with preparation
        // and Reset restoration like every other Workspace operation.
        let _operation = crate::engine::workspace::lock_worker_operation(&self.repository, worker)
            .map_err(|source| WorkerActionError::WorkspaceOperation {
                worker: worker.clone(),
                detail: source.to_string(),
            })?;
        self.commands
            .rebase(&workspace, &branch)
            .map_err(|detail| WorkerActionError::Command {
                operation: "rebase Worker bookmark",
                detail,
            })?;
        if let Err(push) = self.commands.push(&workspace, &branch) {
            let rollback = self.commands.undo(&workspace).err();
            return Err(WorkerActionError::RebasePush {
                branch,
                push,
                rollback,
            });
        }
        Ok(RebaseOutcome { branch })
    }

    /// Opens the pull request for the Worker's bookmark in the default browser.
    pub fn open_pull_request(
        &self,
        worker: &WorkerId,
    ) -> Result<OpenPullRequestOutcome, WorkerActionError> {
        let (branch, _) = self.branch_and_workspace(worker)?;
        let repository = repository_slug(&self.repository);
        if repository.is_empty() {
            return Err(WorkerActionError::RepositoryUnavailable);
        }
        self.commands.open_pull_request(&repository, &branch)?;
        Ok(OpenPullRequestOutcome { branch })
    }

    /// Folds the Worker's working-copy edits into the commits they belong to,
    /// then pushes the rewritten bookmark.
    ///
    /// This is the mechanical half of "address this feedback in the base
    /// commit": the author chooses the destination by starting an edit on top of
    /// it (`jj new <commit>`), and `jj absorb` moves each hunk into the closest
    /// mutable ancestor where those lines last changed. A run that absorbs
    /// nothing does not push, so `absorb` is safe to run speculatively.
    pub fn absorb(&self, worker: &WorkerId) -> Result<AbsorbOutcome, WorkerActionError> {
        let (branch, workspace) = self.branch_and_workspace(worker)?;
        // Absorb rewrites the working copy, so serialise it with preparation and
        // Reset restoration like every other Workspace operation.
        let _operation = crate::engine::workspace::lock_worker_operation(&self.repository, worker)
            .map_err(|source| WorkerActionError::WorkspaceOperation {
                worker: worker.clone(),
                detail: source.to_string(),
            })?;
        let report =
            self.commands
                .absorb(&workspace)
                .map_err(|detail| WorkerActionError::Command {
                    operation: "absorb Worker working-copy edits",
                    detail,
                })?;
        if report.is_empty() {
            return Ok(AbsorbOutcome { branch, report });
        }
        if let Err(push) = self.commands.push(&workspace, &branch) {
            let rollback = self.commands.undo(&workspace).err();
            return Err(WorkerActionError::AbsorbPush {
                branch,
                push,
                rollback,
            });
        }
        Ok(AbsorbOutcome { branch, report })
    }

    /// Opens an interactive provider Session for the Worker in kitty.
    pub fn mount(&self, worker: &WorkerId) -> Result<MountOutcome, WorkerActionError> {
        let snapshot = self.repository.worker_pool().snapshot();
        let state =
            snapshot
                .worker(worker.as_str())
                .ok_or_else(|| WorkerActionError::WorkerNotFound {
                    worker: worker.clone(),
                })?;
        let workspace = crate::engine::workspace::worker_path(self.repository.root(), worker);
        if !workspace.is_dir() {
            return Err(WorkerActionError::MissingWorkspace {
                worker: worker.clone(),
            });
        }
        let profile = state
            .profile()
            .cloned()
            .or_else(|| snapshot.pool().and_then(|pool| pool.profile().cloned()))
            .unwrap_or_else(|| crate::engine::AgentRuntimeProfile::new(AgentRuntime::Claude))
            .with_model_override(self.model.as_ref());
        let runtime = profile.runtime();
        let session = resolve_agent_session_for_runtime(runtime, state.log_file().map(Path::new));
        runtime.probe(&workspace)?;
        runtime.preflight_dispatch_input(profile.model())?;
        let session_directory = self.repository.root().join(".jj/pool/pi-sessions");
        let session_id = match &session {
            AgentSessionResolution::Resumed { session_id } => Some(session_id.as_str()),
            AgentSessionResolution::Fresh { .. } => None,
        };
        let command =
            interactive_agent_command(runtime, profile.model(), &session_directory, session_id)?;
        let tab_id = self.commands.mount(
            worker,
            &workspace,
            &command,
            &self.first_pane_command,
            &self.layout,
        )?;
        // Record the Worker as occupied so Dispatch treats its open tab as
        // unavailable. Best-effort: a failed marker write must not fail Mount.
        let _ = self.repository.state_store().in_use().mark(worker);
        Ok(MountOutcome {
            runtime,
            session,
            tab_id,
        })
    }

    /// Abandons the current Run, releases capacity, and restores the Workspace.
    pub fn reset(&self, worker: &WorkerId) -> Result<ResetOutcome, WorkerActionError> {
        let operation = crate::engine::workspace::lock_worker_operation(&self.repository, worker)
            .map_err(|source| WorkerActionError::WorkspaceOperation {
            worker: worker.clone(),
            detail: source.to_string(),
        })?;
        let run = self.supervisor.reset_run(&self.repository, worker)?;
        let workspace = crate::engine::workspace::worker_path(self.repository.root(), worker);
        let restoration = if workspace.is_dir() {
            let handle_worker = worker.clone();
            let thread_worker = worker.clone();
            let join = thread::Builder::new()
                .name(format!("jjfx-restore-{worker}"))
                .spawn(move || {
                    SystemCommands::restore_workspace(&thread_worker, &workspace, operation)
                })
                .map_err(|source| WorkerActionError::RestorationSpawn {
                    worker: worker.clone(),
                    detail: source.to_string(),
                })?;
            WorkspaceRestoration::Pending(WorkspaceRestorationHandle {
                worker: handle_worker,
                join,
            })
        } else {
            WorkspaceRestoration::SkippedMissingWorkspace
        };
        Ok(ResetOutcome { run, restoration })
    }

    /// Dismisses an idle Worker from the Pool or clears a terminal Worker in place.
    ///
    /// Busy Workers are rejected. Terminal Workers are deliberately cleared
    /// without Workspace restoration so their failed or completed contents can
    /// still be inspected, matching the compatibility TUI behavior.
    pub fn dismiss(&self, worker: &WorkerId) -> Result<DismissOutcome, WorkerActionError> {
        let pool = self.repository.worker_pool();
        let snapshot = pool.reconcile_runs();
        let state =
            snapshot
                .worker(worker.as_str())
                .ok_or_else(|| WorkerActionError::WorkerNotFound {
                    worker: worker.clone(),
                })?;
        match state.status() {
            WorkerStatus::Busy => Err(WorkerActionError::WorkerPool(
                WorkerPoolError::WorkerNotIdle {
                    worker: worker.clone(),
                },
            )),
            WorkerStatus::Idle => {
                let resize = pool.remove(worker.clone())?;
                Ok(DismissOutcome::Removed {
                    capacity: resize.capacity().as_usize(),
                })
            }
            WorkerStatus::Done | WorkerStatus::Failed => {
                pool.clear_terminal(worker)?;
                Ok(DismissOutcome::Reset)
            }
        }
    }

    /// Answers a Worker's pending opencode permission request through the
    /// daemon API.
    ///
    /// A permission request is what leaves an opencode Run needing a human, so
    /// answering it is what lets the Run proceed without mounting its Workspace
    /// in a terminal. Only an opencode Worker with an identified session and a
    /// pending request can be answered; anything else is a typed error the
    /// frontend reports, never a silently dropped action.
    pub fn resolve_permission(
        &self,
        worker: &WorkerId,
        decision: PermissionDecision,
    ) -> Result<PermissionResolution, WorkerActionError> {
        let snapshot = self.repository.worker_pool().snapshot();
        let state =
            snapshot
                .worker(worker.as_str())
                .ok_or_else(|| WorkerActionError::WorkerNotFound {
                    worker: worker.clone(),
                })?;
        if state.agent_runtime() != Some(AgentRuntime::OpenCode) {
            return Err(WorkerActionError::NoRunSession {
                worker: worker.clone(),
            });
        }
        let api = crate::engine::OpenCodeApi::new();
        let session_id = crate::engine::runtime::identify_run_session(
            &api,
            &self.repository,
            worker,
            state.ticket(),
        )
        .ok_or_else(|| WorkerActionError::NoRunSession {
            worker: worker.clone(),
        })?;
        let request = api
            .pending_permissions(&session_id)
            .map_err(|error| WorkerActionError::PermissionRequest {
                worker: worker.clone(),
                detail: error.to_string(),
            })?
            .into_iter()
            .next()
            .ok_or_else(|| WorkerActionError::NoPendingPermission {
                worker: worker.clone(),
            })?;
        api.reply_permission(&session_id, request.id(), decision)
            .map_err(|error| WorkerActionError::PermissionRequest {
                worker: worker.clone(),
                detail: error.to_string(),
            })?;
        Ok(PermissionResolution {
            worker: worker.clone(),
            request,
            decision,
        })
    }

    /// Builds a pull-request review plan and starts it as a Follow-up Run.
    ///
    /// When [`Self::with_review_findings`] supplied a reviewer's findings, they
    /// are inlined alongside the human comments and failing checks.
    pub fn review(
        &self,
        worker: &WorkerId,
        mode: RunMode,
    ) -> Result<FollowUpOutcome, WorkerActionError> {
        let (repository, pull_request) = self.pull_request_for(worker)?;
        let checks = self
            .commands
            .failing_checks(&repository, pull_request.number)?;
        let prompt =
            build_review_prompt(&repository, &pull_request, &checks, &self.review_findings);
        self.follow_up(worker, &prompt, None, mode)
    }

    /// Describes the pull request `review` would review, for a configured AI
    /// reviewer to review without a local checkout.
    pub fn review_request(&self, worker: &WorkerId) -> Result<ReviewRequest, WorkerActionError> {
        let (repository, pull_request) = self.pull_request_for(worker)?;
        let base = if pull_request.base_ref_name.trim().is_empty() {
            "main".to_owned()
        } else {
            pull_request.base_ref_name.clone()
        };
        Ok(ReviewRequest::new(
            repository,
            base,
            pull_request.head_ref_name.clone(),
        ))
    }

    /// Returns the `owner/repo` and open pull request for a Worker's bookmark.
    fn pull_request_for(
        &self,
        worker: &WorkerId,
    ) -> Result<(String, PullRequest), WorkerActionError> {
        let snapshot = self.repository.worker_pool().snapshot();
        let state =
            snapshot
                .worker(worker.as_str())
                .ok_or_else(|| WorkerActionError::WorkerNotFound {
                    worker: worker.clone(),
                })?;
        let branch = state
            .branch_name()
            .filter(|branch| !branch.is_empty())
            .ok_or_else(|| WorkerActionError::MissingBranch {
                worker: worker.clone(),
            })?;
        let repository = repository_slug(&self.repository);
        if repository.is_empty() {
            return Err(WorkerActionError::RepositoryUnavailable);
        }
        let pull_request = self
            .commands
            .pull_request(&repository, branch)?
            .ok_or_else(|| WorkerActionError::PullRequestNotFound {
                branch: branch.to_owned(),
            })?;
        Ok((repository, pull_request))
    }

    fn branch_and_workspace(
        &self,
        worker: &WorkerId,
    ) -> Result<(String, std::path::PathBuf), WorkerActionError> {
        let snapshot = self.repository.worker_pool().snapshot();
        let state =
            snapshot
                .worker(worker.as_str())
                .ok_or_else(|| WorkerActionError::WorkerNotFound {
                    worker: worker.clone(),
                })?;
        let branch = state
            .branch_name()
            .filter(|branch| !branch.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| WorkerActionError::MissingBranch {
                worker: worker.clone(),
            })?;
        let workspace = crate::engine::workspace::worker_path(self.repository.root(), worker);
        if !workspace.is_dir() {
            return Err(WorkerActionError::MissingWorkspace {
                worker: worker.clone(),
            });
        }
        Ok((branch, workspace))
    }

    fn follow_up(
        &self,
        worker: &WorkerId,
        prompt: &str,
        fresh_system_prompt: Option<String>,
        mode: RunMode,
    ) -> Result<FollowUpOutcome, WorkerActionError> {
        // Serialise against Workspace preparation and Reset restoration. Acquiring
        // the lock blocks until any in-flight restoration has finished, so a
        // Follow-up never starts editing a working copy that is still being
        // rewritten. The guard is released once the Run's process has started:
        // holding it for the whole Run would stop Reset from interrupting it.
        let operation = crate::engine::workspace::lock_worker_operation(&self.repository, worker)
            .map_err(|source| WorkerActionError::WorkspaceOperation {
            worker: worker.clone(),
            detail: source.to_string(),
        })?;
        let snapshot = self.repository.worker_pool().snapshot();
        let worker_snapshot =
            snapshot
                .worker(worker.as_str())
                .ok_or_else(|| WorkerActionError::WorkerNotFound {
                    worker: worker.clone(),
                })?;
        let profile = worker_snapshot
            .profile()
            .cloned()
            .or_else(|| snapshot.pool().and_then(|pool| pool.profile().cloned()))
            .unwrap_or_else(|| crate::engine::AgentRuntimeProfile::new(AgentRuntime::Claude))
            .with_model_override(self.model.as_ref());
        profile
            .runtime()
            .preflight_dispatch(profile.model(), self.repository.root())?;
        let (reservation, prior_log) = self
            .repository
            .worker_pool()
            .begin_follow_up(worker, profile)?;
        let runtime = reservation.agent_runtime();
        let session = self.resolve_follow_up_session(
            runtime,
            worker,
            prior_log
                .as_deref()
                .filter(|path| !path.as_os_str().is_empty()),
        );
        let mut invocation = AgentRuntimeInvocation::new(prompt).with_ticket_delivery_profile();
        if let Some(model) = reservation.profile().model() {
            invocation = invocation.with_model(model.clone());
        }
        match &session {
            AgentSessionResolution::Resumed { session_id } => {
                invocation = invocation.with_session_id(session_id);
            }
            AgentSessionResolution::Fresh { .. } => {
                if let Some(system_prompt) = fresh_system_prompt {
                    invocation = invocation.with_system_prompt(system_prompt);
                }
            }
        }
        // An opencode Follow-up to a session jjfx can name is delivered through
        // the daemon API: the prompt reaches the Run's session without a client
        // process, and the Worker's liveness follows the session (ticket 37).
        // Anything the API cannot do - unreachable, too old, or rejected - falls
        // back to today's process invocation rather than fail the Run. A
        // foreground Follow-up always drives a process: the caller waits on it.
        let named_session = match &session {
            AgentSessionResolution::Resumed { session_id } => Some(session_id.clone()),
            AgentSessionResolution::Fresh { .. } => None,
        };
        if mode == RunMode::Background
            && runtime == AgentRuntime::OpenCode
            && let Some(session_id) = named_session.as_deref()
            && self.deliver_session_prompt(
                &reservation,
                session_id,
                &invocation.delivered_prompt(runtime),
            )
        {
            drop(operation);
            return Ok(FollowUpOutcome {
                worker: worker.clone(),
                runtime,
                session,
                execution: FollowUpExecution::Session(FollowUpSession::new(session_id)),
            });
        }
        let execution = match mode {
            RunMode::Foreground => FollowUpExecution::Foreground(
                self.supervisor.run_reserved_foreground_with_handoff(
                    reservation,
                    invocation,
                    move || drop(operation),
                )?,
            ),
            RunMode::Background => {
                let execution = self
                    .supervisor
                    .run_reserved_background(reservation, invocation)?;
                drop(operation);
                FollowUpExecution::Background(Box::new(execution))
            }
        };
        Ok(FollowUpOutcome {
            worker: worker.clone(),
            runtime,
            session,
            execution,
        })
    }

    /// Resolves the Agent Session a Follow-up should drive.
    ///
    /// An opencode Run's session id is recorded in pool-owned sidecar state
    /// (ticket 38), so that exact id is preferred over the Run log: it is
    /// available from the first moment, and an API-native Follow-up does not
    /// rewrite the log a later Run would otherwise read. Every other runtime
    /// resolves from the Run log exactly as before.
    fn resolve_follow_up_session(
        &self,
        runtime: AgentRuntime,
        worker: &WorkerId,
        prior_log: Option<&Path>,
    ) -> AgentSessionResolution {
        if runtime == AgentRuntime::OpenCode
            && let Ok(Some(session_id)) = self.repository.state_store().sessions().session(worker)
            && !session_id.is_empty()
        {
            return AgentSessionResolution::Resumed { session_id };
        }
        resolve_agent_session_for_runtime(runtime, prior_log)
    }

    /// Delivers a Follow-up prompt to an opencode Run's session through the
    /// daemon API, reporting whether the daemon accepted it.
    ///
    /// Best-effort by contract: only an accepted prompt is `true`. An
    /// unreachable or older opencode, or a server rejection, reports `false` so
    /// the caller falls back to the process invocation rather than fail the Run.
    fn deliver_session_prompt(
        &self,
        reservation: &Reservation,
        session_id: &str,
        prompt: &str,
    ) -> bool {
        // Record the session first: a failure here must fall back before the
        // prompt is sent, so a Run is never delivered twice.
        if reservation
            .repository()
            .state_store()
            .sessions()
            .record(reservation.worker_id(), session_id)
            .is_err()
        {
            return false;
        }
        if crate::engine::OpenCodeApi::new()
            .prompt_async(session_id, prompt)
            .is_err()
        {
            return false;
        }
        // The daemon admitted the prompt. Clear the previous Run's provider
        // terminal event, exactly as a spawned client's log truncation does:
        // reconcile must not read that result as this Run's.
        truncate_run_log(reservation.repository(), reservation.worker_id());
        true
    }
}

/// Empties a Worker's Run log so a previous Run's provider terminal event is not
/// read as the current Run's.
///
/// Best-effort: a missing or unwritable log must not fail the Follow-up, because
/// the Run's liveness still follows its session.
fn truncate_run_log(repository: &Repository, worker: &WorkerId) {
    let path = repository
        .root()
        .join(".jj/pool")
        .join(format!("{worker}.log"));
    let _ = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path);
}

#[derive(Debug, Clone, Copy)]
struct SystemCommands;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequest {
    number: u64,
    url: String,
    head_ref_name: String,
    #[serde(default)]
    base_ref_name: String,
    mergeable: String,
    #[serde(default)]
    review_decision: String,
}

#[derive(Debug, Deserialize)]
struct Check {
    name: String,
    conclusion: String,
}

impl SystemCommands {
    /// Build a worker's kitty tab from `layout`. The first pane becomes the
    /// tab; every later pane splits from the window of an earlier one. Role
    /// commands follow the mount's own policy: the configured first-pane
    /// command (else a cleared shell), the agent invocation, and cleared
    /// shells.
    fn mount(
        self,
        worker: &WorkerId,
        workspace: &Path,
        command: &str,
        first_pane_command: &[String],
        layout: &PaneLayout,
    ) -> Result<String, WorkerActionError> {
        let address = kitty_address()?;
        let cwd = format!("--cwd={}", workspace.display());
        let title = worker.as_str();
        let mut windows: Vec<Option<String>> = vec![None; layout.panes.len()];

        for (index, pane) in layout.panes.iter().enumerate() {
            let pane_command = match pane.role {
                PaneRole::First if first_pane_command.is_empty() => shell_command(),
                PaneRole::First => first_pane_command.to_vec(),
                PaneRole::Agent => vec!["zsh".to_string(), "-ic".to_string(), command.to_string()],
                PaneRole::Shell => shell_command(),
            };
            if index == 0 {
                let mut tab_args = vec![
                    "@",
                    &address,
                    "launch",
                    "--type=tab",
                    "--tab-title",
                    title,
                    &cwd,
                    "--",
                ];
                tab_args.extend(pane_command.iter().map(String::as_str));
                let tab_id = self.run("create kitty tab", "kitten", &tab_args)?;
                let tab_id = tab_id.trim().to_owned();
                if tab_id.is_empty() {
                    // The tab exists but has no id to anchor splits to.
                    return Ok(tab_id);
                }
                if layout.panes.len() > 1 {
                    // `--bias` is only a percentage under the splits layout, so
                    // force it before the first split.
                    let _ = self.run(
                        "force kitty splits layout",
                        "kitten",
                        &[
                            "@",
                            &address,
                            "goto-layout",
                            "--match",
                            &format!("window_id:{tab_id}"),
                            "splits",
                        ],
                    );
                }
                windows[0] = Some(tab_id);
                continue;
            }

            let Some(anchor) = windows.get(pane.from).and_then(Option::as_deref) else {
                // The anchor pane never opened; panes hanging off it cannot be
                // placed, so leave the tab as far as it got.
                continue;
            };
            let Some(location) = pane.location else {
                // Validation rejects this; stay tolerant of hand-built plans.
                continue;
            };
            let match_arg = format!("window_id:{anchor}");
            let next_to_arg = format!("id:{anchor}");
            let location_arg = format!("--location={}", location.as_kitty_value());
            let bias_arg = format!("--bias={}", pane.bias);
            let mut split_args = vec![
                "@",
                &address,
                "launch",
                "--match",
                &match_arg,
                "--next-to",
                &next_to_arg,
                &location_arg,
                &bias_arg,
                &cwd,
                "--",
            ];
            split_args.extend(pane_command.iter().map(String::as_str));
            let window = self.run("split kitty tab", "kitten", &split_args)?;
            let window = window.trim().to_owned();
            if window.is_empty() {
                continue;
            }
            windows[index] = Some(window);
        }

        if let Some(agent) = windows.get(layout.focus_pane()).and_then(Option::as_deref) {
            let _ = self.run(
                "focus kitty window",
                "kitten",
                &[
                    "@",
                    &address,
                    "focus-window",
                    "--match",
                    &format!("id:{agent}"),
                ],
            );
        }
        Ok(windows.first().cloned().flatten().unwrap_or_default())
    }

    fn rebase(self, workspace: &Path, branch: &str) -> Result<(), String> {
        self.run_status(workspace, "jj", &["rebase", "-b", branch, "-d", "main"])
    }

    fn push(self, workspace: &Path, branch: &str) -> Result<(), String> {
        self.run_status(workspace, "jj", &["git", "push", "-b", branch])
    }

    /// Runs `jj absorb` and reads what it did from its summary.
    ///
    /// `jj absorb` writes that summary to stderr and exits zero whether or not
    /// anything was absorbed, so the summary - not the exit code - says what
    /// happened.
    fn absorb(self, workspace: &Path) -> Result<AbsorbReport, String> {
        let output = Command::new("jj")
            .args(["absorb"])
            .current_dir(workspace)
            .output()
            .map_err(|source| source.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        Ok(classify_absorb(&String::from_utf8_lossy(&output.stderr)))
    }

    fn undo(self, workspace: &Path) -> Result<(), String> {
        // `jj op undo` was removed; the command is `jj undo`. jjfx shares one op
        // log across workspaces, so allow undoing the last operation even when it
        // was performed in another workspace (jj 0.46 refuses that by default).
        self.run_status(workspace, "jj", &["undo", "--allow-cross-workspace"])
    }

    fn open_pull_request(self, repository: &str, branch: &str) -> Result<(), WorkerActionError> {
        self.run(
            "open pull request",
            "gh",
            &["-R", repository, "pr", "view", branch, "--web"],
        )
        .map(|_| ())
    }

    fn run_status(self, workspace: &Path, program: &str, arguments: &[&str]) -> Result<(), String> {
        let output = Command::new(program)
            .args(arguments)
            .current_dir(workspace)
            .output()
            .map_err(|source| source.to_string())?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
        }
    }

    fn restore_workspace(
        worker: &WorkerId,
        workspace: &Path,
        _operation: crate::engine::workspace::WorkspaceOperationGuard,
    ) -> Result<(), WorkspaceRestorationError> {
        Self::run_workspace_command(worker, workspace, "restore Workspace", &["restore"])?;
        Self::run_workspace_command(worker, workspace, "create fresh change", &["new", "main"])
    }

    fn run_workspace_command(
        worker: &WorkerId,
        workspace: &Path,
        operation: &'static str,
        arguments: &[&str],
    ) -> Result<(), WorkspaceRestorationError> {
        let output = crate::jj::command()
            .args(arguments)
            .current_dir(workspace)
            .output()
            .map_err(|source| WorkspaceRestorationError::Command {
                worker: worker.clone(),
                operation,
                detail: source.to_string(),
            })?;
        if output.status.success() {
            return Ok(());
        }
        Err(WorkspaceRestorationError::Command {
            worker: worker.clone(),
            operation,
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }

    fn pull_request(
        self,
        repository: &str,
        branch: &str,
    ) -> Result<Option<PullRequest>, WorkerActionError> {
        let output = self.run(
            "find pull request",
            "gh",
            &[
                "-R",
                repository,
                "pr",
                "list",
                "--head",
                branch,
                "--json",
                "number,url,headRefName,mergeable,reviewDecision,baseRefName",
                "--limit",
                "1",
            ],
        )?;
        let requests = serde_json::from_str::<Vec<PullRequest>>(&output).map_err(|source| {
            WorkerActionError::Command {
                operation: "decode pull request",
                detail: source.to_string(),
            }
        })?;
        Ok(requests.into_iter().next())
    }

    fn failing_checks(
        self,
        repository: &str,
        number: u64,
    ) -> Result<Vec<Check>, WorkerActionError> {
        let number = number.to_string();
        let output = Command::new("gh")
            .args([
                "-R",
                repository,
                "pr",
                "checks",
                &number,
                "--json",
                "name,conclusion",
            ])
            .output()
            .map_err(|source| WorkerActionError::Command {
                operation: "query pull-request checks",
                detail: source.to_string(),
            })?;
        if !output.status.success() && output.stdout.is_empty() {
            return Err(WorkerActionError::Command {
                operation: "query pull-request checks",
                detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        let checks = serde_json::from_slice::<Vec<Check>>(&output.stdout).map_err(|source| {
            WorkerActionError::Command {
                operation: "decode pull-request checks",
                detail: source.to_string(),
            }
        })?;
        Ok(checks
            .into_iter()
            .filter(|check| {
                matches!(
                    check.conclusion.to_ascii_uppercase().as_str(),
                    "FAILURE" | "STARTUP_FAILURE" | "TIMED_OUT"
                )
            })
            .collect())
    }

    fn run(
        self,
        operation: &'static str,
        program: &str,
        arguments: &[&str],
    ) -> Result<String, WorkerActionError> {
        let output = Command::new(program)
            .args(arguments)
            .output()
            .map_err(|source| WorkerActionError::Command {
                operation,
                detail: source.to_string(),
            })?;
        if !output.status.success() {
            return Err(WorkerActionError::Command {
                operation,
                detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        String::from_utf8(output.stdout).map_err(|source| WorkerActionError::Command {
            operation,
            detail: source.to_string(),
        })
    }
}

/// The command for a plain mount shell: an interactive shell with the
/// scrollback cleared, so a reused tab does not show the previous run.
fn shell_command() -> Vec<String> {
    vec![
        "zsh".to_string(),
        "-ic".to_string(),
        "clear; exec zsh".to_string(),
    ]
}

fn kitty_address() -> Result<String, WorkerActionError> {
    if let Some(address) = env::var_os("KITTY_LISTEN_ON")
        && !address.is_empty()
    {
        return Ok(format!("--to={}", address.to_string_lossy()));
    }
    let entries = fs::read_dir("/tmp").map_err(|source| WorkerActionError::KittyUnavailable {
        detail: source.to_string(),
    })?;
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str()
            && name.starts_with("kitty-visor-")
        {
            return Ok(format!("--to=unix:/tmp/{name}"));
        }
    }
    Err(WorkerActionError::KittyUnavailable {
        detail: "no kitty visor socket found".to_owned(),
    })
}

fn interactive_agent_command(
    runtime: AgentRuntime,
    model: Option<&AgentModel>,
    session_directory: &Path,
    session_id: Option<&str>,
) -> Result<String, WorkerActionError> {
    let command = match (runtime, session_id) {
        (AgentRuntime::Claude, Some(session)) => {
            format!("claude --resume {}; exec zsh", shell_quote(session))
        }
        (AgentRuntime::Claude, None) => "claude; exec zsh".to_owned(),
        (AgentRuntime::Codex, Some(session)) => format!(
            "codex --sandbox workspace-write --ask-for-approval on-request resume {}; exec zsh",
            shell_quote(session)
        ),
        (AgentRuntime::Codex, None) => {
            "codex --sandbox workspace-write --ask-for-approval on-request; exec zsh".to_owned()
        }
        (AgentRuntime::Pi, session) => {
            return Ok(render_shell_command(&pi_interactive_command(
                model,
                session_directory,
                session,
            )?));
        }
        (AgentRuntime::OpenCode, Some(session)) => {
            format!("opencode --session {}; exec zsh", shell_quote(session))
        }
        (AgentRuntime::OpenCode, None) => "opencode; exec zsh".to_owned(),
    };
    Ok(command)
}

fn render_shell_command(command: &Command) -> String {
    let arguments = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|value| shell_quote(&value.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ");
    format!("exec {arguments}")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn build_review_prompt(
    repository: &str,
    pull_request: &PullRequest,
    checks: &[Check],
    findings: &[ReviewFinding],
) -> String {
    let header = if pull_request.url.is_empty() {
        format!("#{}", pull_request.number)
    } else {
        format!("{} (#{})", pull_request.url, pull_request.number)
    };
    let review_state = match pull_request.review_decision.as_str() {
        "APPROVED" => "approved",
        "CHANGES_REQUESTED" => "changes requested",
        "REVIEW_REQUIRED" => "review required",
        _ => "no review decision",
    };
    let mut prompt = format!(
        "Review and address feedback on PR {header}.\nCurrent review state: {review_state}.\n\n"
    );
    let mut step = 1;
    if pull_request.mergeable.eq_ignore_ascii_case("CONFLICTING") {
        prompt.push_str(&format!(
            "{step}. This PR has merge conflicts. Run `jj rebase -d 'trunk()'`, resolve every conflict, then push with `jj git push --named {}=@`.\n\n",
            pull_request.head_ref_name
        ));
        step += 1;
    }
    prompt.push_str(&format!(
        "{step}. Fetch all review comments with `gh -R {repository} pr view {} --comments` and inline threads with `gh api repos/{repository}/pulls/{}/comments`.\n\n",
        pull_request.number, pull_request.number
    ));
    step += 1;
    if !findings.is_empty() {
        prompt.push_str(&format!("{step}. Fix these AI reviewer findings:\n"));
        for finding in findings {
            prompt.push_str(&format!("   - {}\n", finding.summary()));
            for line in finding.body().lines() {
                prompt.push_str(&format!("     {line}\n"));
            }
            for suggestion in finding.suggestions() {
                prompt.push_str(&format!("     Suggested fix: {suggestion}\n"));
            }
        }
        prompt.push('\n');
        step += 1;
    }
    prompt.push_str(&format!(
        "{step}. Address each unresolved comment, or explain a reasoned disagreement in the PR. Decide where each fix belongs before you make it:\n   - A fix to code this PR already introduced belongs in the commit that introduced it - not in a new commit stacked on top, and not in a hand-split. Find that commit with `jj log`, start an edit on top of it with `jj new <commit>`, make the change, then run `jj absorb`. jj moves each hunk into the closest mutable ancestor where those lines last changed, and abandons the edit commit once every hunk has landed (describe the edit if you want to keep it).\n   - `jj absorb` can leave the commits above it conflicted: resolve every conflict it reports, or run `jj undo` and address that feedback on top of `@` instead.\n   - Feedback that does not belong in an existing commit - a new file, a new test - is an ordinary edit on `@`.\n\n"
    ));
    step += 1;
    if !checks.is_empty() {
        prompt.push_str(&format!("{step}. Fix these failing CI checks:\n"));
        for check in checks {
            prompt.push_str(&format!("   - {}\n", check.name));
        }
        prompt.push_str(&format!(
            "   Inspect failures with `gh -R {repository} run list --branch {} --status failure` and `gh -R {repository} run view <run-id> --log-failed`.\n\n",
            pull_request.head_ref_name
        ));
        step += 1;
    }
    prompt.push_str(&format!(
        "{step}. Run linting, type checking, and tests. Describe the change as `<ticket>: address review feedback`, push with `jj git push --named {}=@`, and post a summary with `gh -R {repository} pr comment {} --body \"<summary of changes made>\"`.",
        pull_request.head_ref_name, pull_request.number
    ));
    prompt
}

/// Reads `jj absorb`'s stderr summary.
///
/// The summary is prose, so classify it tolerantly: an unrecognised summary
/// means absorb moved nothing, which is the safe reading - the caller then skips
/// the push it would otherwise make.
fn classify_absorb(stderr: &str) -> AbsorbReport {
    let mut revisions = 0;
    let mut remaining = false;
    for line in stderr.lines() {
        if let Some(count) = line
            .trim()
            .strip_prefix("Absorbed changes into ")
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|count| count.parse::<usize>().ok())
        {
            revisions = count;
        }
        if line.trim_start().starts_with("Remaining changes:") {
            remaining = true;
        }
    }
    AbsorbReport {
        revisions,
        remaining,
    }
}

fn repository_slug(repository: &Repository) -> String {
    repository
        .worker_pool()
        .snapshot()
        .pool()
        .map_or_else(String::new, |pool| pool.gh_repo().to_owned())
}

fn send_system_prompt(repository: &str) -> String {
    format!(
        "You are an autonomous agent in a jj (Jujutsu VCS) workspace.\n\n\
CRITICAL RULES:\n\
- Use jj commands, NEVER git commands.\n\
- The gh CLI requires: gh -R {repository} pr create ...\n\
- To push your work: jj git push --named <branch>=@\n\
- Do NOT ask questions. Make reasonable decisions and proceed."
    )
}

/// Failure to perform a frontend-neutral Worker action.
#[derive(Debug, Error)]
pub enum WorkerActionError {
    /// No idle Worker was available for an arbitrary task.
    #[error("no idle Worker is available")]
    NoIdleWorker,
    /// The requested Worker was not present in the Pool snapshot.
    #[error("Worker {worker} not found")]
    WorkerNotFound { worker: WorkerId },
    /// Review and repository actions require a resolved Worker bookmark.
    #[error("Worker {worker} has no branch")]
    MissingBranch { worker: WorkerId },
    /// The compatible structured log is absent.
    #[error("Worker {worker} has no Run log")]
    MissingLog { worker: WorkerId },
    /// A log cannot be interpreted without its persisted Agent Runtime.
    #[error("Worker {worker} has no Agent Runtime")]
    MissingRuntime { worker: WorkerId },
    /// An operational action requires the provisioned Worker Workspace.
    #[error("Workspace directory is missing for Worker {worker}")]
    MissingWorkspace { worker: WorkerId },
    /// kitty could not be located for Mount.
    #[error("kitty is unavailable: {detail}")]
    KittyUnavailable { detail: String },
    /// The selected runtime's Direct Dispatch profile is not ready.
    #[error(transparent)]
    RuntimePreflight(#[from] AgentRuntimePreflightError),
    /// The selected runtime's interactive command could not be built.
    #[error(transparent)]
    RuntimeCommand(#[from] AgentRuntimeCommandError),
    /// The Worker has no opencode Run session to answer a permission for.
    #[error("Worker {worker} has no opencode Run session")]
    NoRunSession { worker: WorkerId },
    /// The Worker's opencode session has no pending permission request.
    #[error("Worker {worker} has no pending permission request")]
    NoPendingPermission { worker: WorkerId },
    /// The opencode API could not read or answer a permission request.
    #[error("cannot answer Worker {worker}'s permission request: {detail}")]
    PermissionRequest { worker: WorkerId, detail: String },
    /// The Repository has no compatible GitHub slug.
    #[error("cannot detect GitHub repository")]
    RepositoryUnavailable,
    /// No open pull request exists for the Worker bookmark.
    #[error("no pull request found for branch {branch}")]
    PullRequestNotFound { branch: String },
    /// Pushing a rebased bookmark failed after the local operation completed.
    #[error("cannot push rebased branch {branch}: {push}; rollback: {rollback:?}")]
    RebasePush {
        branch: String,
        push: String,
        rollback: Option<String>,
    },
    /// Pushing a bookmark whose history `jj absorb` rewrote failed.
    #[error("cannot push absorbed branch {branch}: {push}; rollback: {rollback:?}")]
    AbsorbPush {
        branch: String,
        push: String,
        rollback: Option<String>,
    },
    /// An external action adapter failed.
    #[error("cannot {operation}: {detail}")]
    Command {
        operation: &'static str,
        detail: String,
    },
    /// A Worker Workspace operation could not acquire its serialization lock.
    #[error("cannot lock Workspace operations for Worker {worker}: {detail}")]
    WorkspaceOperation { worker: WorkerId, detail: String },
    /// The asynchronous Workspace restoration thread could not start.
    #[error("cannot start Workspace restoration for Worker {worker}: {detail}")]
    RestorationSpawn { worker: WorkerId, detail: String },
    /// The Worker lifecycle transition failed.
    #[error(transparent)]
    WorkerPool(#[from] WorkerPoolError),
    /// The Agent Runtime executable required by Mount is unavailable.
    #[error(transparent)]
    Probe(#[from] AgentRuntimeProbeError),
    /// A compatibility state document could not be read.
    #[error(transparent)]
    State(#[from] crate::engine::StateError),
    /// The Agent Runtime could not launch or finalize.
    #[error(transparent)]
    Run(#[from] RunSupervisorError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ReviewSeverity;

    fn pull_request() -> PullRequest {
        PullRequest {
            number: 7,
            url: "https://github.com/o/r/pull/7".to_owned(),
            head_ref_name: "adam/x".to_owned(),
            base_ref_name: "main".to_owned(),
            mergeable: "MERGEABLE".to_owned(),
            review_decision: "REVIEW_REQUIRED".to_owned(),
        }
    }

    fn finding() -> ReviewFinding {
        ReviewFinding::new(
            ReviewSeverity::Major,
            Some("src/x.rs".to_owned()),
            Some((12, 14)),
            Some("Unbounded loop".to_owned()),
            "Cap the loop at the configured pass count.",
            vec!["let n = max_passes.min(3);".to_owned()],
        )
    }

    #[test]
    fn a_reviewers_findings_are_inlined_into_the_prompt() {
        let prompt = build_review_prompt("o/r", &pull_request(), &[], &[finding()]);

        assert!(
            prompt.contains("[major] src/x.rs:12-14 - Unbounded loop"),
            "{prompt}"
        );
        assert!(
            prompt.contains("Cap the loop at the configured pass count."),
            "{prompt}"
        );
        assert!(
            prompt.contains("Suggested fix: let n = max_passes.min(3);"),
            "{prompt}"
        );
    }

    #[test]
    fn no_findings_leaves_the_prompt_without_a_reviewer_step() {
        let prompt = build_review_prompt("o/r", &pull_request(), &[], &[]);

        assert!(!prompt.contains("AI reviewer findings"), "{prompt}");
        assert!(prompt.contains("Fetch all review comments"), "{prompt}");
    }

    #[test]
    fn the_prompt_sends_base_commit_feedback_through_jj_new_and_absorb() {
        let prompt = build_review_prompt("o/r", &pull_request(), &[], &[]);

        assert!(prompt.contains("`jj new <commit>`"), "{prompt}");
        assert!(prompt.contains("`jj absorb`"), "{prompt}");
        // The manual alternatives the prompt must steer away from.
        assert!(prompt.contains("not in a hand-split"), "{prompt}");
        assert!(
            prompt.contains("not in a new commit stacked on top"),
            "{prompt}"
        );
        // Absorb can conflict the commits above it, so the prompt says so.
        assert!(prompt.contains("conflicted"), "{prompt}");
    }

    #[test]
    fn an_absorb_summary_reports_the_revisions_it_filled() {
        let report = classify_absorb(
            "Absorbed changes into 1 revisions:\n  kyzvyv b12347 A: add x\nRebased 1 descendant commits.\n",
        );

        assert_eq!(report.revisions(), 1);
        assert!(!report.has_remaining_changes());
        assert!(!report.is_empty());
    }

    #[test]
    fn an_absorb_summary_reports_hunks_that_stayed_behind() {
        let report = classify_absorb(
            "Absorbed changes into 1 revisions:\n  kpuuuw 06ed0b A: add x\nRemaining changes:\nA y.rs\n",
        );

        assert_eq!(report.revisions(), 1);
        assert!(report.has_remaining_changes());
    }

    #[test]
    fn an_unrecognised_absorb_summary_reads_as_nothing_absorbed() {
        // jj prints this when no ancestor unambiguously owns the changed lines.
        let report = classify_absorb("Nothing changed.\n");

        assert_eq!(report.revisions(), 0);
        assert!(report.is_empty());
        assert!(!report.has_remaining_changes());
    }
}
