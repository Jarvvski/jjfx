//! Frontend-neutral actions over one Worker.

use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread::{self, JoinHandle};

use serde::Deserialize;
use thiserror::Error;

use crate::runtime::pi_interactive_command;
use crate::{
    AgentModel, AgentRuntime, AgentRuntimeCommandError, AgentRuntimeInvocation,
    AgentRuntimePreflightError, AgentRuntimeProbeError, AgentSessionResolution, BackgroundRun,
    CompletedRun, Loaded, Repository, RunLog, RunSupervisor, RunSupervisorError, WorkerId,
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
}

/// The typed result of launching one Follow-up Run.
#[derive(Debug)]
pub struct FollowUpOutcome {
    runtime: AgentRuntime,
    session: AgentSessionResolution,
    execution: FollowUpExecution,
}

impl FollowUpOutcome {
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
    run: crate::RunReset,
    restoration: WorkspaceRestoration,
}

impl ResetOutcome {
    /// Returns how Run cleanup changed the Worker lifecycle.
    pub const fn run(&self) -> crate::RunReset {
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

/// The deep frontend-neutral interface for operational Worker actions.
#[derive(Debug, Clone)]
pub struct WorkerActions {
    repository: Repository,
    supervisor: RunSupervisor,
    commands: SystemCommands,
    model: Option<AgentModel>,
}

impl WorkerActions {
    /// Opens Worker actions for one Repository.
    pub fn new(repository: Repository) -> Self {
        Self {
            repository,
            supervisor: RunSupervisor::new(),
            commands: SystemCommands,
            model: None,
        }
    }

    /// Supplies the provider-aware model profile used by Worker actions.
    ///
    /// Pi requires both provider and model values. Other runtimes retain their
    /// existing provider-managed defaults when no profile is supplied.
    pub fn with_model(mut self, model: impl Into<AgentModel>) -> Self {
        self.model = Some(model.into());
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
        self.follow_up(
            worker,
            prompt.into(),
            Some(send_system_prompt(&repository_slug(&self.repository))),
            mode,
        )
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

    /// Opens an interactive provider Session for the Worker in kitty.
    pub fn mount(&self, worker: &WorkerId) -> Result<MountOutcome, WorkerActionError> {
        let snapshot = self.repository.worker_pool().snapshot();
        let state =
            snapshot
                .worker(worker.as_str())
                .ok_or_else(|| WorkerActionError::WorkerNotFound {
                    worker: worker.clone(),
                })?;
        let workspace = crate::workspace::worker_path(self.repository.root(), worker);
        if !workspace.is_dir() {
            return Err(WorkerActionError::MissingWorkspace {
                worker: worker.clone(),
            });
        }
        let runtime = match state.agent_runtime() {
            Some(runtime) => runtime,
            None => {
                let configured = match self.repository.state_store().pool().load()? {
                    Loaded::Present(versioned) => versioned.value.agent,
                    Loaded::Missing => None,
                };
                AgentRuntime::from_configured(configured.as_ref())
                    .map_err(|value| WorkerPoolError::InvalidAgentRuntime { value })?
            }
        };
        let session = resolve_agent_session_for_runtime(runtime, state.log_file().map(Path::new));
        runtime.probe(&workspace)?;
        let session_directory = self.repository.root().join(".jj/pool/pi-sessions");
        let tab_id = self.commands.mount(
            worker,
            &workspace,
            runtime,
            self.model.as_ref(),
            &session_directory,
            &session,
        )?;
        Ok(MountOutcome {
            runtime,
            session,
            tab_id,
        })
    }

    /// Abandons the current Run, releases capacity, and restores the Workspace.
    pub fn reset(&self, worker: &WorkerId) -> Result<ResetOutcome, WorkerActionError> {
        let operation = crate::workspace::lock_worker_operation(&self.repository, worker).map_err(
            |source| WorkerActionError::WorkspaceOperation {
                worker: worker.clone(),
                detail: source.to_string(),
            },
        )?;
        let run = self.supervisor.reset_run(&self.repository, worker)?;
        let workspace = crate::workspace::worker_path(self.repository.root(), worker);
        let restoration = if workspace.is_dir() {
            let handle_worker = worker.clone();
            let thread_worker = worker.clone();
            let join = thread::Builder::new()
                .name(format!("wsg-restore-{worker}"))
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

    /// Builds a pull-request review plan and starts it as a Follow-up Run.
    pub fn review(
        &self,
        worker: &WorkerId,
        mode: RunMode,
    ) -> Result<FollowUpOutcome, WorkerActionError> {
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
        let checks = self
            .commands
            .failing_checks(&repository, pull_request.number)?;
        let prompt = build_review_prompt(&repository, &pull_request, &checks);
        self.follow_up(worker, prompt, None, mode)
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
        let workspace = crate::workspace::worker_path(self.repository.root(), worker);
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
        prompt: String,
        fresh_system_prompt: Option<String>,
        mode: RunMode,
    ) -> Result<FollowUpOutcome, WorkerActionError> {
        let snapshot = self.repository.worker_pool().snapshot();
        let worker_snapshot =
            snapshot
                .worker(worker.as_str())
                .ok_or_else(|| WorkerActionError::WorkerNotFound {
                    worker: worker.clone(),
                })?;
        let runtime = worker_snapshot
            .agent_runtime()
            .or_else(|| snapshot.pool().and_then(|pool| pool.agent_runtime()))
            .unwrap_or(AgentRuntime::Claude);
        runtime.preflight_dispatch(self.model.as_ref(), self.repository.root())?;
        let (reservation, prior_log) = self.repository.worker_pool().begin_follow_up(worker)?;
        let runtime = reservation.agent_runtime();
        let session = resolve_agent_session_for_runtime(
            runtime,
            prior_log
                .as_deref()
                .filter(|path| !path.as_os_str().is_empty()),
        );
        let mut invocation = AgentRuntimeInvocation::new(prompt).with_ticket_delivery_profile();
        if let Some(model) = self.model.clone() {
            invocation = invocation.with_model(model);
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
        let execution = match mode {
            RunMode::Foreground => FollowUpExecution::Foreground(
                self.supervisor
                    .run_reserved_foreground(reservation, invocation)?,
            ),
            RunMode::Background => FollowUpExecution::Background(Box::new(
                self.supervisor
                    .run_reserved_background(reservation, invocation)?,
            )),
        };
        Ok(FollowUpOutcome {
            runtime,
            session,
            execution,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct SystemCommands;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequest {
    number: u64,
    url: String,
    head_ref_name: String,
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
    fn mount(
        self,
        worker: &WorkerId,
        workspace: &Path,
        runtime: AgentRuntime,
        model: Option<&AgentModel>,
        session_directory: &Path,
        session: &AgentSessionResolution,
    ) -> Result<String, WorkerActionError> {
        let address = kitty_address()?;
        let session_id = match session {
            AgentSessionResolution::Resumed { session_id } => Some(session_id.as_str()),
            AgentSessionResolution::Fresh { .. } => None,
        };
        let command = interactive_agent_command(runtime, model, session_directory, session_id)?;
        let cwd = format!("--cwd={}", workspace.display());
        let title = worker.as_str();
        let tab_id = self.run(
            "create kitty tab",
            "kitten",
            &[
                "@",
                &address,
                "launch",
                "--type=tab",
                "--tab-title",
                title,
                &cwd,
                "--",
                "zsh",
                "-ic",
                &command,
            ],
        )?;
        let tab_id = tab_id.trim().to_owned();
        if !tab_id.is_empty() {
            let match_id = format!("id:{tab_id}");
            let right = self
                .run(
                    "split kitty tab",
                    "kitten",
                    &[
                        "@",
                        &address,
                        "launch",
                        "--match",
                        &match_id,
                        "--location=vsplit",
                        &cwd,
                        "--",
                        "zsh",
                        "-ic",
                        "clear; exec zsh",
                    ],
                )
                .unwrap_or_default();
            let right = right.trim();
            if !right.is_empty() {
                let right_match = format!("id:{right}");
                let _ = self.run(
                    "split kitty pane",
                    "kitten",
                    &[
                        "@",
                        &address,
                        "launch",
                        "--match",
                        &right_match,
                        "--location=hsplit",
                        &cwd,
                        "--",
                        "zsh",
                        "-ic",
                        "clear; exec zsh",
                    ],
                );
            }
            let _ = self.run(
                "focus kitty tab",
                "kitten",
                &["@", &address, "focus-window", "--match", &match_id],
            );
        }
        Ok(tab_id)
    }

    fn rebase(self, workspace: &Path, branch: &str) -> Result<(), String> {
        self.run_status(workspace, "jj", &["rebase", "-b", branch, "-d", "main"])
    }

    fn push(self, workspace: &Path, branch: &str) -> Result<(), String> {
        self.run_status(workspace, "jj", &["git", "push", "-b", branch])
    }

    fn undo(self, workspace: &Path) -> Result<(), String> {
        self.run_status(workspace, "jj", &["op", "undo"])
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
        _operation: crate::workspace::WorkspaceOperationGuard,
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
        let output = Command::new("jj")
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
                "number,url,headRefName,mergeable,reviewDecision",
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

fn build_review_prompt(repository: &str, pull_request: &PullRequest, checks: &[Check]) -> String {
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
    prompt.push_str(&format!(
        "{step}. Address each unresolved comment, or explain a reasoned disagreement in the PR.\n\n"
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
    State(#[from] crate::StateError),
    /// The Agent Runtime could not launch or finalize.
    #[error(transparent)]
    Run(#[from] RunSupervisorError),
}
