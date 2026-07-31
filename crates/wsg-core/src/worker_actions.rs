//! Frontend-neutral actions over one Worker.

use std::process::Command;

use serde::Deserialize;
use thiserror::Error;

use crate::{
    AgentRuntime, AgentRuntimeInvocation, AgentSessionResolution, BackgroundRun, CompletedRun,
    Repository, RunSupervisor, RunSupervisorError, WorkerId, WorkerPoolError,
    resolve_agent_session,
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

/// The deep frontend-neutral interface for operational Worker actions.
#[derive(Debug, Clone)]
pub struct WorkerActions {
    repository: Repository,
    supervisor: RunSupervisor,
    commands: SystemCommands,
}

impl WorkerActions {
    /// Opens Worker actions for one Repository.
    pub fn new(repository: Repository) -> Self {
        Self {
            repository,
            supervisor: RunSupervisor::new(),
            commands: SystemCommands,
        }
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

    fn follow_up(
        &self,
        worker: &WorkerId,
        prompt: String,
        fresh_system_prompt: Option<String>,
        mode: RunMode,
    ) -> Result<FollowUpOutcome, WorkerActionError> {
        let (reservation, prior_log) = self.repository.worker_pool().begin_follow_up(worker)?;
        let session = resolve_agent_session(
            prior_log
                .as_deref()
                .filter(|path| !path.as_os_str().is_empty()),
        );
        let runtime = reservation.agent_runtime();
        let mut invocation = AgentRuntimeInvocation::new(prompt);
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
                    .run_reserved_foreground(&reservation, invocation)?,
            ),
            RunMode::Background => FollowUpExecution::Background(Box::new(
                self.supervisor
                    .run_reserved_background(&reservation, invocation)?,
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
    /// The Repository has no compatible GitHub slug.
    #[error("cannot detect GitHub repository")]
    RepositoryUnavailable,
    /// No open pull request exists for the Worker bookmark.
    #[error("no pull request found for branch {branch}")]
    PullRequestNotFound { branch: String },
    /// An external action adapter failed.
    #[error("cannot {operation}: {detail}")]
    Command {
        operation: &'static str,
        detail: String,
    },
    /// The Worker lifecycle transition failed.
    #[error(transparent)]
    WorkerPool(#[from] WorkerPoolError),
    /// The Agent Runtime could not launch or finalize.
    #[error(transparent)]
    Run(#[from] RunSupervisorError),
}
