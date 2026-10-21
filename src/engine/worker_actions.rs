//! Frontend-neutral actions over one Worker.

use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread::{self, JoinHandle};

use serde::Deserialize;
use thiserror::Error;

use crate::engine::layout::{PaneLayout, PaneRole};
use crate::engine::{
    AgentModel, AgentRuntime, AgentRuntimeInvocation, AgentRuntimeProbeError,
    AgentSessionResolution, BackgroundRun, BackgroundStart, CompletedRun, DispatchExclusions,
    MutationGuard, MutationKind, PermissionDecision, PermissionRequest, Repository, Reservation,
    ReviewFinding, RunLog, RunSupervisor, RunSupervisorError, TicketBranch, TicketId, WorkerId,
    WorkerPoolError, WorkerStatus, resolve_agent_session_for_runtime, resolve_ticket_branch,
};
use crate::jj::PushOutcome;

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
    /// An idle Worker was removed from its Worker Pool.
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

/// A typed reference to one Worker's Run activity, and where it is written.
///
/// A Run normally writes a structured provider log a process owns. A Run
/// delivered through its opencode session spawns no client process at all, so
/// the log it would have written stays empty and the activity lives in the
/// session instead: [`WorkerLogs::session`] names the session to read in that
/// case, and is `None` when the log is the Run's activity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerLogs {
    path: std::path::PathBuf,
    runtime: AgentRuntime,
    session: Option<String>,
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

    /// Returns the opencode session the Run is delivered through, when the Run
    /// writes no process log and drives one.
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
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
        }
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
    /// A runtime that needs an explicit provider keeps it here. Runtimes with
    /// provider-managed defaults retain them when no profile is supplied.
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
            Some(send_system_prompt(
                &self.persisted_gh_repo()?.unwrap_or_default(),
            )),
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
        let system_prompt = send_system_prompt(&self.persisted_gh_repo()?.unwrap_or_default());

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
    ///
    /// A Run delivered through its opencode session writes no process log: the
    /// daemon records its activity in the session, so the log the Run's process
    /// would have written stays empty. The session is named instead, but only
    /// when the log really is empty, so a process-delivered Run keeps the log
    /// that is actually being written.
    pub fn logs(&self, worker: &WorkerId) -> Result<WorkerLogs, WorkerActionError> {
        let snapshot = self.repository.worker_pool().snapshot();
        let state =
            snapshot
                .worker(worker.as_str())
                .ok_or_else(|| WorkerActionError::WorkerNotFound {
                    worker: worker.clone(),
                })?;
        let runtime = state
            .agent_runtime()
            .ok_or_else(|| WorkerActionError::MissingRuntime {
                worker: worker.clone(),
            })?;
        let path = state
            .log_file()
            .filter(|path| !path.is_empty())
            .map(std::path::PathBuf::from);
        if runtime == AgentRuntime::OpenCode
            && path.as_deref().is_none_or(|path| !log_has_content(path))
            && let Some(session_id) = crate::engine::runtime::identify_run_session(
                &crate::engine::OpenCodeApi::new(),
                &self.repository,
                worker,
                state.ticket(),
            )
        {
            return Ok(WorkerLogs {
                path: path.unwrap_or_else(|| self.repository.state_store().worker_log(worker)),
                runtime,
                session: Some(session_id),
            });
        }
        let path = path.ok_or_else(|| WorkerActionError::MissingLog {
            worker: worker.clone(),
        })?;
        if !path.is_file() {
            return Err(WorkerActionError::MissingLog {
                worker: worker.clone(),
            });
        }
        Ok(WorkerLogs {
            path,
            runtime,
            session: None,
        })
    }

    /// Rebases the Worker's bookmark onto `main` and pushes it.
    ///
    /// A rebase can succeed and still record a conflict in the commit it rewrote
    /// (the normal case once `main` has moved under the Worker). jj refuses to
    /// push a conflicted commit, so the push is never attempted and the rebase is
    /// left in place, reported as the state the Workspace is actually in
    /// ([`WorkerActionError::RebaseConflicted`]) rather than as a push failure
    /// whose `rollback` implies a rollback happened. The `jj undo` rollback the
    /// push-failure path uses cannot be trusted here: it undoes the repository's
    /// *last* operation, which in a shared operation log may belong to another
    /// Workspace.
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
        // The rebase moved the Worker's own stack onto `main`; a conflict in that
        // stack is what jj refuses to push.
        if self
            .commands
            .conflicted(&workspace, &branch)
            .map_err(|detail| WorkerActionError::Command {
                operation: "read whether the rebased bookmark has conflicts",
                detail,
            })?
        {
            return Err(WorkerActionError::RebaseConflicted {
                worker: worker.clone(),
                branch,
            });
        }
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
        let Some(repository) = self.persisted_gh_repo()? else {
            return Err(WorkerActionError::RepositoryUnavailable);
        };
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
    ///
    /// The destination is pinned to the Worker's own commits above the mainline,
    /// so a Worker whose change is already committed is a no-op rather than a
    /// rewrite of the branch it sits on, and the mainline is checked before and
    /// after. A repository whose `trunk()` does not resolve is refused outright:
    /// there is nothing for absorb to stop at.
    ///
    /// The commit(s) the Workspace was stacked on are excluded too, from the base
    /// recorded when the dependent was materialised: a Blocker's commit is above
    /// the mainline and mutable, so without that exclusion the fold would rewrite
    /// another Ticket's deliverable.
    pub fn absorb(&self, worker: &WorkerId) -> Result<AbsorbOutcome, WorkerActionError> {
        let (branch, workspace) = self.branch_and_workspace(worker)?;
        // `jj absorb` folds the source revision's hunks into the closest
        // *mutable* ancestor, and a Worker's `@` is its deliverable commit. In a
        // repository whose `trunk()` does not resolve, every ancestor - `main`
        // included - is mutable, so the deliverable would be folded into trunk
        // and trunk rewritten. Check the precondition rather than assume it.
        if !crate::jj::trunk_resolves(&workspace).map_err(|source| WorkerActionError::Command {
            operation: "read the repository's trunk",
            detail: source.to_string(),
        })? {
            return Err(WorkerActionError::TrunkUnresolved {
                worker: worker.clone(),
            });
        }
        // Absorb rewrites the working copy, so serialise it with preparation and
        // Reset restoration like every other Workspace operation. The stack base
        // is read under the lock, so it cannot be replaced by a concurrent
        // re-dispatch between the read and the fold.
        let _operation = crate::engine::workspace::lock_worker_operation(&self.repository, worker)
            .map_err(|source| WorkerActionError::WorkspaceOperation {
                worker: worker.clone(),
                detail: source.to_string(),
            })?;
        let stack_base = self
            .repository
            .state_store()
            .stack_bases()
            .base(worker)
            .map_err(|source| WorkerActionError::Command {
                operation: "read the Worker's stack base",
                detail: source.to_string(),
            })?;
        let report = self
            .commands
            .absorb(&workspace, stack_base.as_deref())
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
        let session_id = match &session {
            AgentSessionResolution::Resumed { session_id } => Some(session_id.as_str()),
            AgentSessionResolution::Fresh { .. } => None,
        };
        let command = interactive_agent_command(runtime, session_id);
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
            let repository = self.repository.clone();
            let join = thread::Builder::new()
                .name(format!("jjfx-restore-{worker}"))
                .spawn(move || {
                    // Record the checkpoint immediately before the Workspace is
                    // restored, so a Reset can be undone from the tool. The guard
                    // completes the record on both the success and failure exits.
                    let _undo = MutationGuard::begin(
                        &repository,
                        thread_worker.as_str(),
                        MutationKind::Reset,
                    );
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

    /// Dismisses an idle Worker from its Worker Pool or clears a terminal Worker in place.
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
    /// The prompt inlines the pull request's review state, its failing checks,
    /// and an instruction to fetch and address human review comments. This is
    /// the review verb's *address step* when it runs with no findings in hand;
    /// [`address_review`](Self::address_review) is the same step carrying one
    /// pass's findings.
    pub fn review(
        &self,
        worker: &WorkerId,
        mode: RunMode,
    ) -> Result<FollowUpOutcome, WorkerActionError> {
        let target = self.review_target(worker)?;
        self.address_review(worker, &target, &[], mode)
    }

    /// Resolves the Pull Request a `review` addresses.
    ///
    /// A repository with no forge, or a Worker with no Pull Request, is refused
    /// here by name, before any pass runs, so the loop never silently reports
    /// convergence for a Pull Request that does not exist.
    pub fn review_target(&self, worker: &WorkerId) -> Result<ReviewTarget, WorkerActionError> {
        let (repository, pull_request) = self.pull_request_for(worker)?;
        let ticket = self.worker_ticket(worker)?;
        Ok(ReviewTarget {
            ticket: ticket.as_str().to_owned(),
            repository,
            pull_request,
        })
    }

    /// Sends one review pass's findings to the Worker as a Follow-up Run.
    ///
    /// This is the loop's address step: it builds the review prompt from the
    /// target Pull Request, its failing checks and the pass's findings. A pass
    /// with no findings never reaches here - the loop converges before it would
    /// address - so the findings list is never empty on this path.
    pub fn address_review(
        &self,
        worker: &WorkerId,
        target: &ReviewTarget,
        findings: &[ReviewFinding],
        mode: RunMode,
    ) -> Result<FollowUpOutcome, WorkerActionError> {
        let checks = self
            .commands
            .failing_checks(&target.repository, target.pull_request.number)?;
        let prompt =
            build_review_prompt(&target.repository, &target.pull_request, &checks, findings);
        self.follow_up(worker, &prompt, None, mode)
    }

    /// Returns the `owner/repo` and open pull request for a Worker's bookmark.
    ///
    /// Only an *open* Pull Request is reviewable, so a branch whose Pull Request
    /// has merged or been closed is refused by naming that state - the answer
    /// `gh pr view` would give - rather than reported as a Pull Request that does
    /// not exist. Only a branch with no Pull Request at all reads as missing.
    fn pull_request_for(
        &self,
        worker: &WorkerId,
    ) -> Result<(String, PullRequest), WorkerActionError> {
        let branch = self.worker_branch(worker)?;
        let Some(repository) = self.persisted_gh_repo()? else {
            return Err(WorkerActionError::RepositoryUnavailable);
        };
        match self.commands.pull_request(&repository, &branch)? {
            PullRequestLookup::Open(pull_request) => Ok((repository, pull_request)),
            PullRequestLookup::NotOpen(state) => {
                Err(WorkerActionError::PullRequestNotOpen { branch, state })
            }
            PullRequestLookup::Missing => Err(WorkerActionError::PullRequestNotFound { branch }),
        }
    }

    /// The Worker Pool's persisted `gh_repo`, or `None` when it records none.
    ///
    /// A slug that could not have come from a GitHub remote - the value a Pool
    /// created before `dogfood/03` still holds - is refused by name here, before
    /// any `gh` call, rather than looked up and reported as a repository that
    /// does not exist. Every Worker verb that resolves the persisted slug goes
    /// through this one read, so none of them can contact `gh` with a
    /// fabricated one.
    fn persisted_gh_repo(&self) -> Result<Option<String>, WorkerActionError> {
        let snapshot = self.repository.worker_pool().snapshot();
        let configured = snapshot.pool().map_or("", |pool| pool.gh_repo());
        if configured.is_empty() {
            return Ok(None);
        }
        crate::engine::workspace::refuse_a_fabricated_slug(self.repository.root(), configured)
            .map_err(|error| WorkerActionError::RepositoryIdentity {
                detail: error.to_string(),
            })?;
        Ok(Some(configured.to_owned()))
    }

    /// The Worker's delivered bookmark, resolved against the repository.
    ///
    /// Worker state records the bare Ticket id at reservation time
    /// (`branch_name: ticket.to_lowercase()`), but the dispatch prompt tells the
    /// agent to create `{prefix}/{ticket}-<description>`. The state value is
    /// therefore a guess at the bookmark, never the bookmark itself: resolve the
    /// real one by matching on the Ticket, the same rule orchestration uses
    /// ([`resolve_ticket_branch`]). Resolution happens at *use* rather than from
    /// a recorded name because before the agent has chosen its bookmark there is
    /// nothing to record. It is a read, so it fails before any Workspace is
    /// touched.
    fn worker_branch(&self, worker: &WorkerId) -> Result<String, WorkerActionError> {
        let ticket = self.worker_ticket(worker)?;
        match resolve_ticket_branch(self.repository.root(), &ticket) {
            TicketBranch::Resolved(branch) => Ok(branch),
            TicketBranch::Missing => Err(WorkerActionError::BranchNotFound {
                worker: worker.clone(),
                ticket: ticket.as_str().to_owned(),
            }),
            TicketBranch::Ambiguous(branches) => Err(WorkerActionError::AmbiguousBranch {
                worker: worker.clone(),
                ticket: ticket.as_str().to_owned(),
                branches: branches.join(", "),
            }),
        }
    }

    /// The Ticket a Worker was reserved for, parsed from its state.
    ///
    /// A Worker that was never reserved for a Ticket has none, which is the same
    /// refusal as a missing branch: the verbs that resolve a branch cannot act
    /// without one.
    fn worker_ticket(&self, worker: &WorkerId) -> Result<TicketId, WorkerActionError> {
        let snapshot = self.repository.worker_pool().snapshot();
        let state =
            snapshot
                .worker(worker.as_str())
                .ok_or_else(|| WorkerActionError::WorkerNotFound {
                    worker: worker.clone(),
                })?;
        state
            .ticket()
            .filter(|ticket| !ticket.is_empty())
            .and_then(|ticket| TicketId::parse(ticket).ok())
            .ok_or_else(|| WorkerActionError::MissingBranch {
                worker: worker.clone(),
            })
    }

    fn branch_and_workspace(
        &self,
        worker: &WorkerId,
    ) -> Result<(String, std::path::PathBuf), WorkerActionError> {
        let branch = self.worker_branch(worker)?;
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
                let started = self
                    .supervisor
                    .run_reserved_background(reservation, invocation)?;
                drop(operation);
                match started {
                    BackgroundStart::Process(run) => FollowUpExecution::Background(run),
                    BackgroundStart::Session(session) => {
                        FollowUpExecution::Session(FollowUpSession::new(session.session_id()))
                    }
                }
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
        // prompt is sent, so a Run is never delivered twice. The baseline the
        // session already carried is recorded with it, so the Run's own spend is
        // recoverable when the session reports its cumulative total.
        if crate::engine::runtime::record_run_session(
            reservation.repository(),
            reservation.worker_id(),
            session_id,
        )
        .is_err()
        {
            return false;
        }
        crate::engine::runtime::deliver_session_prompt(reservation, session_id, prompt)
    }
}

#[derive(Debug, Clone, Copy)]
struct SystemCommands;

#[derive(Debug, Clone, Deserialize)]
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

/// The state `gh` reports for a Pull Request.
///
/// `gh`'s `state` field is `OPEN`, `MERGED` or `CLOSED` (the same three values
/// its `--state` flag accepts). The review verb resolves the *open* Pull
/// Request, because a merged or closed one has nothing left to review, but names
/// the state it found rather than reporting a Pull Request that does not exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum PullRequestState {
    /// The Pull Request is open and reviewable.
    Open,
    /// The Pull Request was merged.
    Merged,
    /// The Pull Request was closed without merging.
    Closed,
}

impl std::fmt::Display for PullRequestState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Open => "open",
            Self::Merged => "merged",
            Self::Closed => "closed",
        })
    }
}

/// The result of resolving a Worker bookmark's Pull Request.
enum PullRequestLookup {
    /// The open Pull Request, ready to review.
    Open(PullRequest),
    /// A Pull Request names the branch, but it is no longer open.
    NotOpen(PullRequestState),
    /// No Pull Request names the branch, in any state.
    Missing,
}

/// One `state` entry of `gh pr list --json state`, which the state-only lookup
/// decodes.
#[derive(Debug, Deserialize)]
struct PullRequestStateRow {
    state: PullRequestState,
}

/// The Pull Request a `review` addresses, resolved once for a whole loop.
///
/// The loop resolves the target once and carries it into every pass, so the
/// Worker's branch and the Pull Request are read once rather than once per pass.
/// It is also where a repository with no forge, or a Worker with no Pull Request,
/// is refused by name - before any pass runs.
#[derive(Debug, Clone)]
pub struct ReviewTarget {
    ticket: String,
    repository: String,
    pull_request: PullRequest,
}

impl ReviewTarget {
    /// Returns the Worker's Ticket, which keys each review pass.
    pub fn ticket(&self) -> &str {
        &self.ticket
    }

    /// Returns the `owner/repo` the Pull Request lives in.
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// Returns the Pull Request number.
    pub const fn number(&self) -> u64 {
        self.pull_request.number
    }

    /// Returns the base ref the Pull Request targets.
    pub fn base(&self) -> &str {
        &self.pull_request.base_ref_name
    }

    /// Returns the head ref the Pull Request is from.
    pub fn branch(&self) -> &str {
        &self.pull_request.head_ref_name
    }
}

/// The `--json` fields [`SystemCommands::failing_checks`] asks `gh` for.
///
/// `gh` changed the field that carries a check's outcome: releases up to 2.x
/// report `conclusion` (`SUCCESS`/`FAILURE`/...), while 2.102 dropped it for
/// `bucket` (`pass`/`fail`/`pending`/`skipping`/`cancel`) and `state`. Requesting
/// a field this `gh` does not have fails the call outright, so the request names
/// only the current pair; [`Check`] still decodes `conclusion` if a body carries
/// it.
const CHECK_FIELDS: &str = "name,bucket,state";

#[derive(Debug, Deserialize)]
struct Check {
    name: String,
    #[serde(default)]
    bucket: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
}

impl Check {
    /// The check's outcome, from whichever field this `gh` filled in.
    ///
    /// `bucket` is preferred, then `state`, then the legacy `conclusion`; an
    /// unrecognised value falls through to the next field. `None` means the body
    /// carried none of them, which is the shape [`parse_checks`] refuses.
    fn outcome(&self) -> Option<CheckOutcome> {
        self.bucket
            .as_deref()
            .and_then(CheckOutcome::from_bucket)
            .or_else(|| self.state.as_deref().and_then(CheckOutcome::from_state))
            .or_else(|| {
                self.conclusion
                    .as_deref()
                    .and_then(CheckOutcome::from_state)
            })
    }

    /// Whether the check ran and failed - the only outcome `review` acts on.
    fn is_failing(&self) -> bool {
        matches!(self.outcome(), Some(CheckOutcome::Fail))
    }
}

/// A check's outcome, normalised across the fields `gh` has reported it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckOutcome {
    /// The check passed, or reported nothing actionable.
    Pass,
    /// The check ran and failed.
    Fail,
    /// The check has not finished.
    Pending,
    /// The check was skipped.
    Skipping,
    /// The check was cancelled.
    Cancel,
}

impl CheckOutcome {
    /// Decodes one of `gh`'s `bucket` values.
    fn from_bucket(bucket: &str) -> Option<Self> {
        match bucket.to_ascii_lowercase().as_str() {
            "pass" => Some(Self::Pass),
            "fail" => Some(Self::Fail),
            "pending" => Some(Self::Pending),
            "skipping" => Some(Self::Skipping),
            "cancel" => Some(Self::Cancel),
            _ => None,
        }
    }

    /// Decodes a `state`, which is the check-run conclusion the legacy
    /// `conclusion` field reported verbatim.
    fn from_state(state: &str) -> Option<Self> {
        match state.to_ascii_uppercase().as_str() {
            "SUCCESS" | "NEUTRAL" => Some(Self::Pass),
            "FAILURE" | "ACTION_REQUIRED" | "TIMED_OUT" | "STARTUP_FAILURE" => Some(Self::Fail),
            "PENDING" | "QUEUED" | "IN_PROGRESS" | "WAITING" | "REQUESTED" => Some(Self::Pending),
            "SKIPPED" | "STALE" => Some(Self::Skipping),
            "CANCELLED" => Some(Self::Cancel),
            _ => None,
        }
    }
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

    /// Whether the rebased bookmark's own stack - everything the rebase moved
    /// above `main` - carries an unresolved conflict.
    ///
    /// The chain above `main`, not just the bookmark's commit: jj records a
    /// conflict in the commit it rewrote, and a push of the bookmark carries
    /// every commit above `main` whether or not the tip itself is the owner.
    fn conflicted(self, workspace: &Path, branch: &str) -> Result<bool, String> {
        let revset = format!("::{} ~ ::main", crate::jj::revset_symbol(branch));
        crate::jj::conflicted(workspace, &revset).map_err(|error| error.to_string())
    }

    fn push(self, workspace: &Path, branch: &str) -> Result<(), String> {
        // `jj git push -b <name>` exits zero and pushes nothing both when the
        // bookmark is already published and when the name matches no bookmark,
        // so the exit code alone cannot tell them apart. `push_bookmark`
        // classifies from the remote-bookmark state *and* jj's bookmark list:
        // `Nothing` is a bookmark that is there and already published - the
        // desired state already holds, so the verb succeeds and nothing is
        // rolled back - while a name that matches no bookmark is a failure.
        match crate::jj::push_bookmark(workspace, branch).map_err(|error| error.to_string())? {
            PushOutcome::Pushed | PushOutcome::Nothing => Ok(()),
            PushOutcome::Failed(detail) => Err(detail),
        }
    }

    /// Runs `jj absorb` and reads what it did from its summary.
    ///
    /// `jj absorb` writes that summary to stderr and exits zero whether or not
    /// anything was absorbed, so the summary - not the exit code - says what
    /// happened.
    ///
    /// The destination is pinned to the Worker's own mutable commits above the
    /// mainline ([`crate::jj::absorb_destinations`]) rather than jj's default
    /// `mutable()`, which is only safe while trunk is immutable. `stack_base` is
    /// the commit(s) the Workspace was stacked on, excluded the same way: a
    /// Blocker's commit is above the mainline and mutable, so it would otherwise
    /// take the fold and rewrite another Ticket's pull request. The mainline is
    /// then re-read: if it moved anyway, the absorb is rolled back with
    /// `jj undo` and reported as a failure, never as a success.
    fn absorb(
        self,
        workspace: &Path,
        stack_base: Option<&[String]>,
    ) -> Result<AbsorbReport, String> {
        let mainline =
            crate::jj::mainline_commit(workspace).map_err(|source| source.to_string())?;
        let destinations = crate::jj::absorb_destinations(stack_base);
        let output = crate::jj::command()
            .args(["absorb", "--into", &destinations])
            .current_dir(workspace)
            .output()
            .map_err(|source| source.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        let report = classify_absorb(&String::from_utf8_lossy(&output.stderr));
        let after = crate::jj::mainline_commit(workspace).map_err(|source| source.to_string())?;
        if after != mainline {
            return Err(match self.undo(workspace) {
                Ok(()) => format!(
                    "jj absorb rewrote the mainline ({mainline} -> {after}); rolled back with `jj undo`"
                ),
                Err(rollback) => format!(
                    "jj absorb rewrote the mainline ({mainline} -> {after}) and the `jj undo` rollback failed: {rollback}"
                ),
            });
        }
        Ok(report)
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

    /// Resolves the branch's Pull Request, naming whether it is open, another
    /// state, or absent.
    ///
    /// The open lookup runs first so an open Pull Request is the one reviewed
    /// even when an older Pull Request on the same branch merged. Only when no
    /// open Pull Request exists does the state lookup run, so the common case
    /// costs one `gh` call.
    fn pull_request(
        self,
        repository: &str,
        branch: &str,
    ) -> Result<PullRequestLookup, WorkerActionError> {
        if let Some(pull_request) = self.find_open_pull_request(repository, branch)? {
            return Ok(PullRequestLookup::Open(pull_request));
        }
        match self.pull_request_state(repository, branch)? {
            Some(state) => Ok(PullRequestLookup::NotOpen(state)),
            None => Ok(PullRequestLookup::Missing),
        }
    }

    /// The branch's open Pull Request, or `None` when it has none open.
    fn find_open_pull_request(
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
                "number,url,headRefName,baseRefName,mergeable,reviewDecision",
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

    /// The state `gh` reports for the branch's most recent Pull Request in any
    /// state, or `None` when the branch has no Pull Request at all.
    fn pull_request_state(
        self,
        repository: &str,
        branch: &str,
    ) -> Result<Option<PullRequestState>, WorkerActionError> {
        let output = self.run(
            "find pull request state",
            "gh",
            &[
                "-R", repository, "pr", "list", "--head", branch, "--state", "all", "--json",
                "state", "--limit", "1",
            ],
        )?;
        let rows = serde_json::from_str::<Vec<PullRequestStateRow>>(&output).map_err(|source| {
            WorkerActionError::Command {
                operation: "decode pull request state",
                detail: source.to_string(),
            }
        })?;
        Ok(rows.into_iter().next().map(|row| row.state))
    }

    fn failing_checks(
        self,
        repository: &str,
        number: u64,
    ) -> Result<Vec<Check>, WorkerActionError> {
        let number = number.to_string();
        let command = format!("gh -R {repository} pr checks {number} --json {CHECK_FIELDS}");
        let output = Command::new("gh")
            .args([
                "-R",
                repository,
                "pr",
                "checks",
                &number,
                "--json",
                CHECK_FIELDS,
            ])
            .output()
            .map_err(|source| WorkerActionError::Command {
                operation: "query pull-request checks",
                detail: source.to_string(),
            })?;
        // `gh pr checks` exits non-zero when a check has failed, but still
        // prints the checks as JSON, so only an empty stdout is a real failure.
        if !output.status.success() && output.stdout.is_empty() {
            return Err(WorkerActionError::Command {
                operation: "query pull-request checks",
                detail: format!(
                    "`{command}` failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            });
        }
        Ok(parse_checks(&command, &output.stdout)?
            .into_iter()
            .filter(Check::is_failing)
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

/// Whether a Run log has anything in it yet.
///
/// A Run delivered through its opencode session leaves its log empty, so an
/// empty log is what tells the two apart. A log that cannot be read counts as
/// empty: reading the session is the better answer, and the session reader
/// reports its own failure plainly.
fn log_has_content(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.len() > 0)
        .unwrap_or(false)
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

fn interactive_agent_command(runtime: AgentRuntime, session_id: Option<&str>) -> String {
    match (runtime, session_id) {
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
        (AgentRuntime::OpenCode, Some(session)) => {
            format!("opencode --session {}; exec zsh", shell_quote(session))
        }
        (AgentRuntime::OpenCode, None) => "opencode; exec zsh".to_owned(),
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Decodes a `gh pr checks` body into the checks it reports.
///
/// `command` is the invocation the body came from; it is quoted back in every
/// error so a `gh` whose output jjfx cannot read is diagnosable from the message
/// alone. A body carrying none of the outcome fields `gh` has used - a shape
/// from an unexpected `gh` - is refused rather than read as "no failing checks",
/// which would send a Worker to review a pull request whose failures it never
/// saw. `gh`'s own output is included so the mismatch can be seen directly.
fn parse_checks(command: &str, body: &[u8]) -> Result<Vec<Check>, WorkerActionError> {
    let raw = String::from_utf8_lossy(body);
    let checks = serde_json::from_str::<Vec<Check>>(raw.as_ref()).map_err(|source| {
        WorkerActionError::Command {
            operation: "query pull-request checks",
            detail: format!(
                "`{command}` returned a shape jjfx cannot read ({source}); output: {}",
                raw.trim()
            ),
        }
    })?;
    if let Some(check) = checks.iter().find(|check| check.outcome().is_none()) {
        return Err(WorkerActionError::Command {
            operation: "query pull-request checks",
            detail: format!(
                "`{command}` returned a check without a `bucket`, `state` or `conclusion` field: \
                 {}; output: {}",
                check.name,
                raw.trim()
            ),
        });
    }
    Ok(checks)
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
    if findings.is_empty() {
        prompt.push_str(&format!(
            "{step}. Fetch all review comments with `gh -R {repository} pr view {} --comments` and inline threads with `gh api repos/{repository}/pulls/{}/comments`.\n\n",
            pull_request.number, pull_request.number
        ));
    } else {
        prompt.push_str(&format!(
            "{step}. Address each of these unresolved review threads:\n"
        ));
        prompt.push_str(&render_findings(findings));
        prompt.push('\n');
    }
    step += 1;
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

/// Renders a pass's findings as the review prompt's thread list.
///
/// Each finding is one bullet - its severity, location and title - with the
/// thread's comments and any suggested change beneath it, indented so the
/// Worker reads each thread as one unit.
fn render_findings(findings: &[ReviewFinding]) -> String {
    let mut rendered = String::new();
    for finding in findings {
        rendered.push_str(&format!("   - {}\n", finding.summary()));
        for line in finding.body().lines() {
            rendered.push_str(&format!("     {line}\n"));
        }
        for suggestion in finding.suggestions() {
            rendered.push_str("     suggested change:\n");
            for line in suggestion.lines() {
                rendered.push_str(&format!("       {line}\n"));
            }
        }
    }
    rendered
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
    /// The requested Worker was not present in the Worker Pool snapshot.
    #[error("Worker {worker} not found")]
    WorkerNotFound { worker: WorkerId },
    /// Review and repository actions require a resolved Worker bookmark.
    #[error("Worker {worker} has no branch")]
    MissingBranch { worker: WorkerId },
    /// No bookmark in the repository names the Worker's Ticket.
    #[error(
        "no bookmark names Worker {worker}'s Ticket {ticket}: create \
         `<prefix>/{ticket}-<description>` or re-run the Run that was to deliver it"
    )]
    BranchNotFound { worker: WorkerId, ticket: String },
    /// More than one bookmark names the Worker's Ticket.
    #[error(
        "Worker {worker}'s Ticket {ticket} is named by more than one bookmark ({branches}): \
         keep one and delete the rest"
    )]
    AmbiguousBranch {
        worker: WorkerId,
        ticket: String,
        branches: String,
    },
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
    /// The Worker Pool's persisted `gh_repo` could not have come from a GitHub
    /// remote, so the repository it names cannot be resolved.
    #[error("cannot resolve GitHub repository: {detail}")]
    RepositoryIdentity { detail: String },
    /// No pull request exists for the Worker bookmark, in any state.
    #[error("no pull request found for branch {branch}")]
    PullRequestNotFound { branch: String },
    /// The Worker's pull request exists but is no longer open, so there is
    /// nothing left to review. The state found is named - `merged` or `closed` -
    /// rather than reported as a pull request that does not exist.
    #[error("pull request for branch {branch} is {state}; there is nothing left to review")]
    PullRequestNotOpen {
        branch: String,
        state: PullRequestState,
    },
    /// The rebase succeeded but left the bookmark conflicted, so jj will not
    /// push it. The rebase is deliberately left in place and the state is named,
    /// rather than reported as a push failure with a rollback that did not happen.
    #[error(
        "rebased branch {branch} onto main, but it has conflicts to resolve: resolve them in \
         Worker {worker}'s Workspace (`jj resolve`), then re-run to push; the rebase is left in \
         place"
    )]
    RebaseConflicted { worker: WorkerId, branch: String },
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
    /// The repository has no resolvable trunk, so absorb has nothing safe to
    /// stop at.
    #[error(
        "cannot absorb for Worker {worker}: the repository has no resolvable trunk, so every \
         commit - `main` included - is mutable and `jj absorb` would fold the Worker's commit \
         into it; fetch the mainline first (`jj git fetch`, or push `main` to `origin`)"
    )]
    TrunkUnresolved { worker: WorkerId },
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

    #[test]
    fn the_prompt_has_no_reviewer_step_and_fetches_human_comments() {
        let prompt = build_review_prompt("o/r", &pull_request(), &[], &[]);

        assert!(!prompt.contains("AI reviewer findings"), "{prompt}");
        assert!(prompt.contains("Fetch all review comments"), "{prompt}");
    }

    /// A pass with findings lists them, so the address step sends the review the
    /// loop read rather than only a pointer at the PR's comments.
    #[test]
    fn the_prompt_lists_a_pass_findings() {
        let findings = vec![ReviewFinding::new(
            crate::engine::ReviewSeverity::Major,
            Some("src/x.rs".to_owned()),
            Some((12, 14)),
            Some("Cap the loop".to_owned()),
            "@adam: This can spin forever.",
            vec!["n = 3".to_owned()],
        )];
        let prompt = build_review_prompt("o/r", &pull_request(), &[], &findings);

        assert!(prompt.contains("unresolved review threads"), "{prompt}");
        assert!(
            prompt.contains("[major] src/x.rs:12-14 - Cap the loop"),
            "{prompt}"
        );
        assert!(prompt.contains("@adam: This can spin forever."), "{prompt}");
        assert!(prompt.contains("n = 3"), "{prompt}");
        // With findings in hand the prompt does not send the Worker back to
        // re-fetch what the loop already read.
        assert!(!prompt.contains("Fetch all review comments"), "{prompt}");
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

    /// `gh` reports a Pull Request's state as `OPEN`, `MERGED` or `CLOSED`; the
    /// refusal's vocabulary is exactly that state, so the decode must read all
    /// three rather than guess.
    #[test]
    fn a_pull_request_state_decodes_ghs_vocabulary() {
        let decode = |body: &str| {
            serde_json::from_str::<PullRequestStateRow>(body)
                .expect("a gh state decodes")
                .state
        };

        assert_eq!(decode(r#"{"state":"OPEN"}"#), PullRequestState::Open);
        assert_eq!(decode(r#"{"state":"MERGED"}"#), PullRequestState::Merged);
        assert_eq!(decode(r#"{"state":"CLOSED"}"#), PullRequestState::Closed);
    }

    /// A Pull Request that is no longer open is refused by naming the state
    /// `gh` found, so a merged or closed one never reads as one that does not
    /// exist.
    #[test]
    fn the_not_open_refusal_names_the_state() {
        let merged = WorkerActionError::PullRequestNotOpen {
            branch: "adam/x".to_owned(),
            state: PullRequestState::Merged,
        };
        assert!(merged.to_string().contains("is merged"), "{merged}");

        let closed = WorkerActionError::PullRequestNotOpen {
            branch: "adam/x".to_owned(),
            state: PullRequestState::Closed,
        };
        assert!(closed.to_string().contains("is closed"), "{closed}");
    }

    /// The body `gh -R Jarvvski/jjfx pr checks 163 --json name,bucket,state`
    /// returned on gh 2.102.0, captured 2026-10-20.
    const CAPTURED_CHECKS: &str = include_str!("../../tests/fixtures/gh/pr-checks-2.102.json");

    /// The command string `failing_checks` quotes back in any parse error.
    const CHECKS_COMMAND: &str = "gh -R o/r pr checks 7 --json name,bucket,state";

    /// The parse must read the body a current `gh` actually returns - a shape
    /// with no `conclusion` field - rather than reject it. This pins it against
    /// a captured body so a future gh rename is caught here.
    #[test]
    fn the_parse_reads_a_captured_current_gh_checks_body() {
        let checks = parse_checks(CHECKS_COMMAND, CAPTURED_CHECKS.as_bytes())
            .expect("the captured body parses");

        assert_eq!(checks.len(), 1, "the captured body reports one check");
        assert_eq!(checks[0].name, "check");
        assert_eq!(checks[0].outcome(), Some(CheckOutcome::Pass));
        assert!(!checks[0].is_failing());
    }

    /// The failing check is the one `review` acts on, read from `bucket`, while
    /// a passing check is left alone.
    #[test]
    fn a_failing_bucket_is_the_check_review_acts_on() {
        let checks = parse_checks(
            CHECKS_COMMAND,
            br#"[{"bucket":"fail","name":"tests","state":"FAILURE"},
                 {"bucket":"pass","name":"lint","state":"SUCCESS"}]"#,
        )
        .expect("a current-gh body parses");

        let failing: Vec<&str> = checks
            .iter()
            .filter(|check| check.is_failing())
            .map(|check| check.name.as_str())
            .collect();
        assert_eq!(failing, vec!["tests"]);
    }

    /// A body with only `state` - no `bucket` - still classifies, so the parse
    /// does not hinge on a single field name.
    #[test]
    fn a_state_only_body_still_classifies() {
        let checks = parse_checks(CHECKS_COMMAND, br#"[{"name":"tests","state":"FAILURE"}]"#)
            .expect("a state-only body parses");

        assert!(checks[0].is_failing());
    }

    /// The legacy shape - the `conclusion` field gh 2.102 dropped - keeps
    /// decoding, so a body captured from an older gh is not a hard failure.
    #[test]
    fn the_legacy_conclusion_field_still_decodes() {
        let checks = parse_checks(
            CHECKS_COMMAND,
            br#"[{"conclusion":"FAILURE","name":"tests"}]"#,
        )
        .expect("a legacy body parses");

        assert!(checks[0].is_failing());
    }

    /// An all-passing set yields no failing checks.
    #[test]
    fn an_all_passing_set_reports_no_failing_checks() {
        let checks = parse_checks(
            CHECKS_COMMAND,
            br#"[{"bucket":"pass","name":"tests","state":"SUCCESS"},
                 {"bucket":"pass","name":"lint","state":"SUCCESS"}]"#,
        )
        .expect("a current-gh body parses");

        assert!(!checks.iter().any(Check::is_failing));
    }

    /// An empty set is a valid body - a pull request with no checks - not an
    /// unrecognised shape.
    #[test]
    fn an_empty_checks_body_is_no_failing_checks() {
        let checks = parse_checks(CHECKS_COMMAND, b"[]").expect("an empty body parses");

        assert!(checks.is_empty());
    }

    /// A body with none of the outcome fields is refused by name, quoting the
    /// command and gh's own output, rather than read as "no failing checks".
    #[test]
    fn an_unrecognised_checks_shape_names_the_command_and_gh_output() {
        let body = br#"[{"name":"tests"}]"#;
        let error = parse_checks(CHECKS_COMMAND, body).expect_err("the shape is refused");
        let message = error.to_string();

        assert!(message.contains(CHECKS_COMMAND), "{message}");
        assert!(message.contains(r#"[{"name":"tests"}]"#), "{message}");
    }

    /// A body that is not a checks array at all is refused the same way - the
    /// command and the raw output, never a bare serde error.
    #[test]
    fn a_body_that_is_not_a_checks_array_names_the_command_and_gh_output() {
        let error =
            parse_checks(CHECKS_COMMAND, b"not json").expect_err("a non-array body is refused");
        let message = error.to_string();

        assert!(message.contains(CHECKS_COMMAND), "{message}");
        assert!(message.contains("not json"), "{message}");
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

    /// Run one jj command in `dir`, asserting it succeeds. Goes through
    /// [`crate::jj::command`] so a throwaway test commit is never signed with
    /// the developer's key.
    fn jj(dir: &Path, args: &[&str]) {
        let output = crate::jj::command()
            .current_dir(dir)
            .args(args)
            .output()
            .expect("jj should be installed");
        assert!(
            output.status.success(),
            "jj {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// The commit id `revset` resolves to in `dir`.
    fn commit_id(dir: &Path, revset: &str) -> String {
        crate::jj::read_in_dir(dir, &["log", "-r", revset, "--no-graph", "-T", "commit_id"])
            .expect("a commit id read")
            .trim()
            .to_owned()
    }

    /// The contents of `path` at `revset` in `dir`.
    fn file(dir: &Path, revset: &str, path: &str) -> String {
        crate::jj::read_in_dir(dir, &["file", "show", "-r", revset, path]).expect("a file read")
    }

    /// The repository `jjfx absorb` was observed to destroy: a *local* `main`
    /// bookmark with no `main@origin` fetched, so `trunk()` does not resolve and
    /// jj calls every commit - `main` included - mutable. `@` is a Worker's
    /// committed deliverable, one change stacked on `main`.
    fn mutable_main_repo(tag: &str) -> std::path::PathBuf {
        let root = crate::store::test_local_repo(tag);
        std::fs::write(root.join("site.css"), "body { color: black; }\n").expect("base file");
        jj(&root, &["describe", "-m", "Lantern static site"]);
        jj(&root, &["bookmark", "create", "main", "-r", "@"]);
        jj(&root, &["new"]);
        std::fs::write(
            root.join("site.css"),
            "body { color: black; }\nform { margin: 1em; }\n",
        )
        .expect("Worker edit");
        jj(
            &root,
            &["describe", "-m", "contact-form: Add a contact form"],
        );
        root
    }

    #[test]
    fn absorb_never_folds_a_committed_change_into_a_mutable_main() {
        let root = mutable_main_repo("absorb-mutable-main");
        let main = commit_id(&root, "main");
        let worker = commit_id(&root, "@");

        // The destination is pinned to the Worker's own commits, so a change
        // already committed as the deliverable has nowhere to go: absorb is the
        // no-op it should be, rather than the rewrite of `main` it used to be.
        let report = SystemCommands.absorb(&root, None).expect("absorb runs");

        assert!(report.is_empty(), "nothing belongs in an earlier commit");
        assert_eq!(
            commit_id(&root, "main"),
            main,
            "`main` must not be rewritten"
        );
        assert_eq!(
            commit_id(&root, "@"),
            worker,
            "the Worker's commit must survive"
        );
        assert_eq!(
            file(&root, "@", "site.css"),
            "body { color: black; }\nform { margin: 1em; }\n",
            "the Worker's change must stay in its commit"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn absorb_still_folds_an_edit_into_the_workers_own_base_commit() {
        // The review-feedback flow the pinning must not break: an edit made on
        // top of a base commit is folded into that commit, and nothing below it
        // moves.
        let root = crate::store::test_local_repo("absorb-base-commit");
        std::fs::write(root.join("seed.txt"), "seed\n").expect("seed file");
        jj(&root, &["describe", "-m", "main: initial"]);
        jj(&root, &["bookmark", "create", "main", "-r", "@"]);
        jj(&root, &["new", "-m", "A: add a and b"]);
        std::fs::write(root.join("a.rs"), "let a = 1;\n").expect("a.rs");
        std::fs::write(root.join("b.rs"), "let b = 2;\n").expect("b.rs");
        jj(&root, &["describe", "-m", "A: add a and b"]);
        let base = commit_id(&root, "@");
        let base_change =
            crate::jj::read_in_dir(&root, &["log", "-r", "@", "--no-graph", "-T", "change_id"])
                .expect("a change id read")
                .trim()
                .to_owned();
        jj(&root, &["new", "-m", "B: bump b"]);
        std::fs::write(root.join("b.rs"), "let b = 3;\n").expect("b.rs edit");
        jj(&root, &["describe", "-m", "B: bump b"]);
        jj(&root, &["new"]);
        std::fs::write(root.join("a.rs"), "let a = 0;\n").expect("feedback edit");
        let main = commit_id(&root, "main");

        let report = SystemCommands.absorb(&root, None).expect("absorb runs");

        assert_eq!(report.revisions(), 1, "the edit folds into one commit");
        assert_eq!(commit_id(&root, "main"), main, "`main` must not move");
        assert_ne!(
            commit_id(&root, &base_change),
            base,
            "the base commit is rewritten by the fold"
        );
        assert_eq!(
            file(&root, &base_change, "a.rs"),
            "let a = 0;\n",
            "the feedback lands in the base commit"
        );
        assert_eq!(
            file(&root, &base_change, "b.rs"),
            "let b = 2;\n",
            "the base commit keeps its own change"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// The corruption one level down from the mutable-main case: a dependent
    /// Worker materialised stacked on its Blocker's commit. The Blocker's commit
    /// is above the mainline and mutable, so it is a legal `jj absorb`
    /// destination unless the recorded stack base excludes it - and folding into
    /// it rewrites a change that belongs to another Ticket's pull request.
    #[test]
    fn absorb_never_folds_a_stacked_workers_change_into_its_blockers_commit() {
        let root = crate::store::test_local_repo("absorb-stacked-worker");
        std::fs::write(root.join("seed.txt"), "seed\n").expect("seed file");
        jj(&root, &["describe", "-m", "main: initial"]);
        jj(&root, &["bookmark", "create", "main", "-r", "@"]);

        // Ticket 01's deliverable: a commit above the mainline, on its own
        // bookmark, as a Blocker's is. The bookmark is the moving handle a
        // rewrite would drag - a commit id would resolve to itself either way.
        jj(&root, &["new", "-m", "01: add the contact form"]);
        std::fs::write(root.join("form.rs"), "fn form() {}\n").expect("01's file");
        jj(&root, &["describe", "-m", "01: add the contact form"]);
        jj(
            &root,
            &[
                "bookmark",
                "create",
                "adam/dogfood--01-contact-form",
                "-r",
                "@",
            ],
        );
        let blocker = commit_id(&root, "adam/dogfood--01-contact-form");

        // Ticket 02, materialised stacked on 01's commit: `prepare_for_dispatch`
        // runs `jj new <Blocker's branch>` and records the parents it lands on.
        jj(&root, &["new", "-m", "02: add validation"]);
        std::fs::write(root.join("validation.rs"), "fn validate() {}\n").expect("02's file");
        jj(&root, &["describe", "-m", "02: add validation"]);
        jj(
            &root,
            &[
                "bookmark",
                "create",
                "adam/dogfood--02-validation",
                "-r",
                "@",
            ],
        );
        let dependent = commit_id(&root, "adam/dogfood--02-validation");
        assert_ne!(blocker, dependent);

        // Feedback for 02 that edits a line 01 introduced. This is exactly the
        // hunk jj folds into the closest mutable ancestor - 01's commit - if the
        // stack base is not excluded.
        jj(&root, &["new"]);
        std::fs::write(root.join("form.rs"), "fn form() -> bool { true }\n")
            .expect("feedback edit");

        let report = SystemCommands
            .absorb(&root, Some(std::slice::from_ref(&blocker)))
            .expect("absorb runs");

        assert_eq!(
            commit_id(&root, "adam/dogfood--01-contact-form"),
            blocker,
            "01's deliverable - another Ticket's pull request - must not be rewritten"
        );
        assert_eq!(
            file(&root, "adam/dogfood--01-contact-form", "form.rs"),
            "fn form() {}\n",
            "01's own change must stay in 01's commit"
        );
        assert_eq!(
            commit_id(&root, "adam/dogfood--02-validation"),
            dependent,
            "02's own commit must not be rewritten either"
        );
        assert_eq!(
            file(&root, "@", "form.rs"),
            "fn form() -> bool { true }\n",
            "with no destination above the base the edit stays in the source revision"
        );
        assert!(
            report.is_empty(),
            "nothing above the base owns the edited lines, so absorb folds nothing: {report:?}"
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
