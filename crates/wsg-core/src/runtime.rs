//! Agent Runtime identity and execution capability probing.

use std::env;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};
use serde::Deserialize;
use thiserror::Error;

use crate::pool::RunClearing;
use crate::{
    Repository, Reservation, RunResult, RunResultSource, StateError, StateRevision, WireAgent,
    WorkerId, WorkerPoolError, WorkerState,
};

const PROCESS_GROUP_GRACE: Duration = Duration::from_secs(1);
const PROCESS_GROUP_POLL: Duration = Duration::from_millis(10);
const PROCESS_GROUP_FORCE_TIMEOUT: Duration = Duration::from_secs(1);
const PI_MCP_ADAPTER_NAME: &str = "pi-mcp-adapter";
const PI_MCP_ADAPTER_VERSION: &str = "2.11.0";
const PI_MCP_ADAPTER_ENTRY: &str = "index.ts";
const PI_PROFILE_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const PI_PROFILE_PROBE_OUTPUT_ENV: &str = "JJFX_PI_PROFILE_PROBE_OUTPUT";
const PI_LINEAR_TOOLS: [&str; 3] = [
    "linear_get_issue",
    "linear_update_issue",
    "linear_create_comment",
];
const PI_DIRECT_DISPATCH_GUIDANCE: &str = "Linear delivery tools are direct Pi tools for this Run. Call linear_get_issue directly to fetch the Ticket, linear_update_issue directly to change status or assignee, and linear_create_comment directly to post delivery notes. Do not look for an MCP wrapper or another tool namespace.";
const PI_PROFILE_PROBE_EXTENSION: &str = r#"import { writeFileSync } from "node:fs";
export default function (pi) {
  pi.on("session_start", () => {
    const output = process.env.JJFX_PI_PROFILE_PROBE_OUTPUT;
    if (!output) throw new Error("missing profile output path");
    writeFileSync(output, JSON.stringify({
      allTools: pi.getAllTools(),
      activeTools: pi.getActiveTools(),
    }));
  });
}
"#;
const DELEGATION_RULES: &str = "Delegated work is read-only.\n\n- Use in-session background tasks or subagents only for independent exploration, documentation lookup, test or log analysis, or review.\n- Explicitly tell every subagent not to edit tracked files or run jj commands.\n- Do not use detached sessions, nested delegation, or worktree or workspace creation.\n- Await all delegated work before finishing.\n- If delegation is unavailable or fails, continue the work directly.\n- The main agent alone owns tracked edits, jj operations, verification, and delivery.";

/// The Agent Runtime recorded for a Worker and selected for a Run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRuntime {
    /// Anthropic's Claude Code runtime.
    Claude,
    /// OpenAI's Codex runtime.
    Codex,
    /// The Pi coding-agent runtime.
    Pi,
}

/// A provider-aware model selection supplied to an Agent Runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentModel {
    provider: Option<String>,
    model: String,
}

impl AgentModel {
    /// Creates a model selection without a provider override.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            provider: None,
            model: model.into(),
        }
    }

    /// Adds the provider required by runtimes such as Pi.
    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// Returns the optional provider identifier.
    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    /// Returns the model identifier.
    pub fn model(&self) -> &str {
        &self.model
    }
}

impl From<&str> for AgentModel {
    fn from(model: &str) -> Self {
        Self::new(model)
    }
}

impl From<String> for AgentModel {
    fn from(model: String) -> Self {
        Self::new(model)
    }
}

/// Typed inputs for one Agent Runtime invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRuntimeInvocation {
    prompt: String,
    model: Option<AgentModel>,
    max_budget_usd: Option<u32>,
    session_id: Option<String>,
    session_directory: Option<PathBuf>,
    name: Option<String>,
    system_prompt: Option<String>,
    direct_dispatch_profile: bool,
}

impl AgentRuntimeInvocation {
    /// Creates a fresh invocation with the required workload prompt.
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            model: None,
            max_budget_usd: None,
            session_id: None,
            session_directory: None,
            name: None,
            system_prompt: None,
            direct_dispatch_profile: false,
        }
    }

    /// Adds a model selection to the invocation.
    pub fn with_model(mut self, model: impl Into<AgentModel>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub(crate) fn with_max_budget_usd(mut self, dollars: u32) -> Self {
        self.max_budget_usd = Some(dollars);
        self
    }

    /// Adds an Agent Session to resume.
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Selects the private session directory used by runtimes that persist
    /// session state outside the Worker Run log.
    pub fn with_session_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.session_directory = Some(directory.into());
        self
    }

    /// Adds a display name for a fresh Agent Runtime invocation.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Adds a system prompt for a fresh invocation.
    pub fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(system_prompt.into());
        self
    }

    pub(crate) fn with_direct_dispatch_profile(mut self) -> Self {
        self.direct_dispatch_profile = true;
        self
    }

    fn session_prompts(&self) -> (Option<String>, String) {
        let prompt = if self.direct_dispatch_profile {
            format!("{PI_DIRECT_DISPATCH_GUIDANCE}\n\n{}", self.prompt)
        } else {
            self.prompt.clone()
        };
        if self
            .session_id
            .as_deref()
            .is_some_and(|session_id| !session_id.is_empty())
        {
            return (None, format!("{DELEGATION_RULES}\n\n{prompt}"));
        }
        let system_prompt = self
            .system_prompt
            .as_deref()
            .filter(|prompt| !prompt.is_empty())
            .map_or_else(
                || DELEGATION_RULES.to_owned(),
                |prompt| format!("{prompt}\n\n{DELEGATION_RULES}"),
            );
        (Some(system_prompt), prompt)
    }
}

/// Inputs required to execute one Run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    runtime: AgentRuntime,
    invocation: AgentRuntimeInvocation,
    workspace: PathBuf,
    log_path: PathBuf,
}

impl RunRequest {
    /// Creates a Run request for a Worker Workspace.
    pub fn new(
        runtime: AgentRuntime,
        invocation: AgentRuntimeInvocation,
        workspace: impl Into<PathBuf>,
        log_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            runtime,
            invocation,
            workspace: workspace.into(),
            log_path: log_path.into(),
        }
    }
}

/// Supervises Agent Runtime Runs through the shared execution seam.
#[derive(Debug, Default, Clone, Copy)]
pub struct RunSupervisor;

impl RunSupervisor {
    /// Creates a Run supervisor.
    pub const fn new() -> Self {
        Self
    }

    /// Executes one Run attached to the caller's terminal.
    pub fn run_foreground(&self, request: &RunRequest) -> Result<RunOutcome, RunSupervisorError> {
        self.run_foreground_started(request, |_| Ok(()))
    }

    fn run_foreground_started(
        &self,
        request: &RunRequest,
        started: impl FnOnce(u32) -> Result<(), RunSupervisorError>,
    ) -> Result<RunOutcome, RunSupervisorError> {
        let capabilities = request.runtime.probe(&request.workspace)?;
        let log = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&request.log_path)
            .map_err(|source| RunSupervisorError::Log {
                path: request.log_path.clone(),
                source,
            })?;
        let log = Arc::new(Mutex::new(log));
        let mut command = request
            .runtime
            .command(&request.invocation, capabilities)
            .map_err(|source| RunSupervisorError::Command {
                runtime: request.runtime,
                source,
            })?;
        command
            .current_dir(&request.workspace)
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|source| RunSupervisorError::Spawn {
                runtime: request.runtime,
                source,
            })?;
        if let Err(error) = started(child.id()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let stdout = child.stdout.take().expect("foreground stdout was piped");
        let stderr = child.stderr.take().expect("foreground stderr was piped");
        let stdout_forwarder =
            spawn_output_forwarder(stdout, io::stdout(), Arc::clone(&log), "stdout");
        let stderr_forwarder =
            spawn_output_forwarder(stderr, io::stderr(), Arc::clone(&log), "stderr");

        let (status, wait_error) = match child.wait() {
            Ok(status) => (Some(status), None),
            Err(source) => {
                let _ = child.kill();
                let _ = child.wait();
                (None, Some(source))
            }
        };
        let stdout_result = join_output_forwarder(stdout_forwarder, "stdout");
        let stderr_result = join_output_forwarder(stderr_forwarder, "stderr");

        stdout_result?;
        stderr_result?;
        if let Some(source) = wait_error {
            return Err(RunSupervisorError::Wait {
                runtime: request.runtime,
                source,
            });
        }

        Ok(RunOutcome {
            exit_code: status.and_then(|status| status.code()),
        })
    }

    /// Executes a reserved Run and finalizes its Worker through the shared waiter.
    pub fn run_reserved_foreground(
        &self,
        reservation: Reservation,
        invocation: AgentRuntimeInvocation,
    ) -> Result<CompletedRun, RunSupervisorError> {
        self.run_reserved_foreground_with_handoff(reservation, invocation, || {})
    }

    pub(crate) fn run_reserved_foreground_with_handoff(
        &self,
        reservation: Reservation,
        invocation: AgentRuntimeInvocation,
        handoff: impl FnOnce(),
    ) -> Result<CompletedRun, RunSupervisorError> {
        let worker = reservation.worker_id().clone();
        let request = reserved_request(&reservation, invocation);
        let mut persisted_revision = None;
        let outcome = match self.run_foreground_started(&request, |pid| {
            let revision = match reservation.persist_pid(pid) {
                Ok(crate::pool::PidPersistence::Persisted(revision)) => revision,
                Ok(crate::pool::PidPersistence::Missing) => {
                    return Err(RunSupervisorError::PersistPidMissing {
                        worker: worker.clone(),
                        pid,
                    });
                }
                Ok(crate::pool::PidPersistence::Conflict) => {
                    return Err(RunSupervisorError::PersistPidConflict {
                        worker: worker.clone(),
                    });
                }
                Err(source) => {
                    return Err(RunSupervisorError::PersistPid {
                        worker: worker.clone(),
                        pid,
                        source,
                    });
                }
            };
            persisted_revision = Some(revision);
            handoff();
            Ok(())
        }) {
            Ok(outcome) => outcome,
            Err(error) => return release_after_failure(&reservation, error),
        };
        let Some(revision) = persisted_revision else {
            return Err(RunSupervisorError::PersistPidConflict { worker });
        };
        RunCompletion::new(reservation, revision).finalize(outcome)
    }

    /// Starts a reserved Run and persists its process identifier before success.
    pub fn run_reserved_background(
        &self,
        reservation: Reservation,
        invocation: AgentRuntimeInvocation,
    ) -> Result<BackgroundRun, RunSupervisorError> {
        let worker_id = reservation.worker_id().clone();
        let request = reserved_request(&reservation, invocation);
        let mut background = match self.run_background(&request) {
            Ok(background) => background,
            Err(error) => return release_after_failure(&reservation, error),
        };
        let pid = background.pid();
        match reservation.persist_pid(pid) {
            Ok(crate::pool::PidPersistence::Persisted(revision)) => {
                background.completion = Some(RunCompletion::new(reservation, revision));
                Ok(background)
            }
            Ok(crate::pool::PidPersistence::Missing) => persist_pid_failure(
                &reservation,
                background,
                RunSupervisorError::PersistPidMissing {
                    worker: worker_id,
                    pid,
                },
            ),
            Ok(crate::pool::PidPersistence::Conflict) => persist_pid_failure(
                &reservation,
                background,
                RunSupervisorError::PersistPidConflict { worker: worker_id },
            ),
            Err(source) => persist_pid_failure(
                &reservation,
                background,
                RunSupervisorError::PersistPid {
                    worker: worker_id,
                    pid,
                    source,
                },
            ),
        }
    }

    /// Abandons a Worker's current Run and returns the Worker to idle.
    ///
    /// This is the only operation that ends a Run that is still executing. It
    /// terminates the recorded Agent Runtime process group, waiting for a
    /// graceful exit before forcing one, then clears the Run from Worker state.
    /// No state lock is held while the process group is signaled.
    ///
    /// An idle Worker, a Worker whose Run already finished, and a Worker whose
    /// recorded process is already gone are all reset without an error. A Run
    /// that a newer Reservation already replaced is reported as superseded and
    /// left untouched.
    pub fn reset_run(
        &self,
        repository: &Repository,
        worker: &WorkerId,
    ) -> Result<RunReset, RunSupervisorError> {
        let pool = repository.worker_pool();
        let reset = |source| RunSupervisorError::Reset {
            worker: worker.clone(),
            source,
        };
        let Some(target) = pool.run_target(worker).map_err(reset)? else {
            return Ok(RunReset::AlreadyIdle);
        };
        let terminated_pid = match target.pid().and_then(live_process_group) {
            Some(pid) => {
                terminate_process_group(pid, PROCESS_GROUP_GRACE).map_err(|source| {
                    RunSupervisorError::ResetCleanup {
                        worker: worker.clone(),
                        pid: pid.as_raw_nonzero().get().unsigned_abs(),
                        source,
                    }
                })?;
                Some(pid.as_raw_nonzero().get().unsigned_abs())
            }
            None => None,
        };
        match pool.clear_run(worker, target).map_err(reset)? {
            RunClearing::Cleared => Ok(RunReset::Abandoned { terminated_pid }),
            RunClearing::AlreadyIdle => Ok(RunReset::AlreadyIdle),
            RunClearing::Superseded => Ok(RunReset::Superseded),
        }
    }

    /// Terminates a persisted background Run without changing Worker state.
    ///
    /// Pool destruction uses this after atomically detaching membership. No
    /// state lock is held while the process group is signaled.
    pub(crate) fn terminate_recorded_process(&self, pid: u32) -> io::Result<()> {
        let pid = i32::try_from(pid)
            .ok()
            .and_then(Pid::from_raw)
            .ok_or_else(|| io::Error::other("recorded Run PID does not fit a Unix process ID"))?;
        match test_kill_process_group(pid) {
            Ok(()) => terminate_process_group(pid, PROCESS_GROUP_GRACE),
            Err(error) if error == rustix::io::Errno::SRCH => Ok(()),
            Err(error) => Err(io::Error::new(
                error.kind(),
                format!("cannot probe recorded process group {pid}: {error}"),
            )),
        }
    }

    /// Starts one Run detached from the caller's terminal.
    pub fn run_background(
        &self,
        request: &RunRequest,
    ) -> Result<BackgroundRun, RunSupervisorError> {
        let capabilities = request.runtime.probe(&request.workspace)?;
        let log = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&request.log_path)
            .map_err(|source| RunSupervisorError::BackgroundLog {
                path: request.log_path.clone(),
                source,
            })?;
        let error_log = log
            .try_clone()
            .map_err(|source| RunSupervisorError::BackgroundLog {
                path: request.log_path.clone(),
                source,
            })?;
        let mut command = request
            .runtime
            .command(&request.invocation, capabilities)
            .map_err(|source| RunSupervisorError::Command {
                runtime: request.runtime,
                source,
            })?;
        command
            .current_dir(&request.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(error_log))
            .process_group(0);
        let child = command
            .spawn()
            .map_err(|source| RunSupervisorError::BackgroundSpawn {
                runtime: request.runtime,
                source,
            })?;
        Ok(BackgroundRun {
            child,
            runtime: request.runtime,
            log_path: request.log_path.clone(),
            completion: None,
        })
    }
}

/// One running background Agent Runtime process.
#[must_use = "background Runs must be waited on so their process is reaped"]
#[derive(Debug)]
pub struct BackgroundRun {
    child: Child,
    runtime: AgentRuntime,
    log_path: PathBuf,
    completion: Option<RunCompletion>,
}

#[derive(Debug)]
struct RunCompletion {
    reservation: Reservation,
    revision: StateRevision<WorkerState>,
}

impl RunCompletion {
    fn new(reservation: Reservation, revision: StateRevision<WorkerState>) -> Self {
        Self {
            reservation,
            revision,
        }
    }

    fn finalize(self, outcome: RunOutcome) -> Result<CompletedRun, RunSupervisorError> {
        let worker = self.reservation.worker_id().clone();
        let log_path = self
            .reservation
            .repository()
            .root()
            .join(".jj/pool")
            .join(format!("{worker}.log"));
        let (result, source) =
            crate::run_log::result_for_finalization(&log_path, self.reservation.agent_runtime());
        self.reservation
            .finalize(self.revision, &result)
            .map_err(|source| RunSupervisorError::Finalize { worker, source })?;
        Ok(CompletedRun {
            process: outcome,
            result,
            result_source: source,
        })
    }
}

impl BackgroundRun {
    /// Returns the process ID, which is also the Run's process-group ID.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Waits for the background Run and reaps its process.
    pub fn wait(mut self) -> Result<CompletedRun, RunSupervisorError> {
        let status = self
            .child
            .wait()
            .map_err(|source| RunSupervisorError::Wait {
                runtime: self.runtime,
                source,
            })?;
        let outcome = RunOutcome {
            exit_code: status.code(),
        };
        if let Some(completion) = self.completion.take() {
            return completion.finalize(outcome);
        }
        let (result, result_source) =
            crate::run_log::result_for_finalization(&self.log_path, self.runtime);
        Ok(CompletedRun {
            process: outcome,
            result,
            result_source,
        })
    }
}

/// A reaped Run together with its provider-neutral terminal result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedRun {
    process: RunOutcome,
    result: RunResult,
    result_source: RunResultSource,
}

impl CompletedRun {
    /// Returns the operating-system exit code, or `None` when signaled.
    pub fn exit_code(&self) -> Option<i32> {
        self.process.exit_code()
    }

    /// Returns the provider-neutral terminal result used for finalization.
    pub fn result(&self) -> &RunResult {
        &self.result
    }

    /// Returns whether the result came from the provider or the fallback path.
    pub fn result_source(&self) -> &RunResultSource {
        &self.result_source
    }
}

/// The result of abandoning a Worker's Run through [`RunSupervisor::reset_run`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunReset {
    /// The Run was ended and the Worker is idle again.
    Abandoned {
        /// The process group that was terminated, absent when no live Agent
        /// Runtime process remained.
        terminated_pid: Option<u32>,
    },
    /// The Worker held no Run to abandon and is idle.
    AlreadyIdle,
    /// A newer Run owns the Worker, so the requested Run was left untouched.
    Superseded,
}

/// The process completion result of an Agent Runtime Run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunOutcome {
    exit_code: Option<i32>,
}

impl RunOutcome {
    /// Returns the process exit code, or `None` when the process was signaled.
    pub fn exit_code(self) -> Option<i32> {
        self.exit_code
    }
}

/// Errors from Agent Runtime Run execution.
#[derive(Debug, Error)]
pub enum RunSupervisorError {
    /// The Agent Runtime capability probe could not start.
    #[error(transparent)]
    Probe(#[from] AgentRuntimeProbeError),
    /// The selected runtime rejected its typed invocation before spawning.
    #[error("cannot build {runtime} Run command: {source}")]
    Command {
        /// Runtime whose command could not be built.
        runtime: AgentRuntime,
        /// Command validation failure.
        #[source]
        source: AgentRuntimeCommandError,
    },
    /// The shared Run log could not be created or truncated.
    #[error("cannot create foreground Run log {path}: {source}")]
    Log {
        /// The configured log path.
        path: PathBuf,
        /// The filesystem error.
        #[source]
        source: io::Error,
    },
    /// The background Run log could not be created or duplicated.
    #[error("cannot create background Run log {path}: {source}")]
    BackgroundLog {
        /// The configured log path.
        path: PathBuf,
        /// The filesystem error.
        #[source]
        source: io::Error,
    },
    /// The Agent Runtime process could not be started.
    #[error("cannot spawn {runtime} foreground Run: {source}")]
    Spawn {
        /// The selected Agent Runtime.
        runtime: AgentRuntime,
        /// The process creation error.
        #[source]
        source: io::Error,
    },
    /// The background Agent Runtime process could not be started.
    #[error("cannot spawn {runtime} background Run: {source}")]
    BackgroundSpawn {
        /// The selected Agent Runtime.
        runtime: AgentRuntime,
        /// The process creation error.
        #[source]
        source: io::Error,
    },
    /// The Worker terminal state could not be persisted.
    #[error("cannot finalize Run for Worker {worker}: {source}")]
    Finalize {
        /// The Worker whose Run completed.
        worker: crate::WorkerId,
        /// The Worker state repository failure.
        #[source]
        source: WorkerPoolError,
    },
    /// The Worker PID could not be loaded or persisted.
    #[error("cannot persist PID for Worker {worker}: {source}")]
    PersistPid {
        /// The Worker whose Run was launched.
        worker: crate::WorkerId,
        /// The process identifier that could not be recorded.
        pid: u32,
        /// The state repository failure.
        #[source]
        source: StateError,
    },
    /// The Worker state disappeared before PID persistence.
    #[error("cannot persist PID for Worker {worker}: Worker state is missing")]
    PersistPidMissing {
        /// The Worker whose Run was launched.
        worker: crate::WorkerId,
        /// The process identifier that could not be recorded.
        pid: u32,
    },
    /// Another mutation replaced the reserved Worker state before PID persistence.
    #[error("cannot persist PID for Worker {worker}: Worker state changed after reservation")]
    PersistPidConflict {
        /// The Worker whose Run was launched.
        worker: crate::WorkerId,
    },
    /// The untracked process group could not be cleaned up after persistence failed.
    #[error("cannot clean up untracked Run for Worker {worker}: {source}")]
    PersistPidCleanup {
        /// The Worker whose Run was launched.
        worker: crate::WorkerId,
        /// The original persistence failure.
        primary: Box<RunSupervisorError>,
        /// The process cleanup failure.
        #[source]
        source: io::Error,
    },
    /// The Worker's Run could not be read or cleared while resetting it.
    #[error("cannot reset Run for Worker {worker}: {source}")]
    Reset {
        /// The Worker whose Run was being abandoned.
        worker: WorkerId,
        /// The Worker Pool state failure.
        #[source]
        source: WorkerPoolError,
    },
    /// The Agent Runtime process group survived Reset, so the Worker keeps its Run.
    #[error("cannot terminate Run process group {pid} for Worker {worker}: {source}")]
    ResetCleanup {
        /// The Worker whose Run was being abandoned.
        worker: WorkerId,
        /// The process group that could not be terminated.
        pid: u32,
        /// The process cleanup failure.
        #[source]
        source: io::Error,
    },
    /// The Reservation could not be released after a failed launch.
    #[error("{primary}; cannot release Reservation for Worker {worker}: {detail}")]
    ReservationRelease {
        /// The Worker whose Reservation was leaked.
        worker: crate::WorkerId,
        /// The spawned process identifier, if launch reached that stage.
        pid: Option<u32>,
        /// The original launch or persistence failure.
        primary: Box<RunSupervisorError>,
        /// The release failure.
        detail: String,
    },
    /// Output could not be forwarded or mirrored.
    #[error("cannot forward {stream} output: {source}")]
    Forward {
        /// The output stream that failed.
        stream: &'static str,
        /// The I/O error.
        #[source]
        source: io::Error,
    },
    /// The Agent Runtime process could not be waited on.
    #[error("cannot wait for {runtime} Run: {source}")]
    Wait {
        /// The selected Agent Runtime.
        runtime: AgentRuntime,
        /// The wait error.
        #[source]
        source: io::Error,
    },
}

fn release_after_failure<T>(
    reservation: &Reservation,
    primary: RunSupervisorError,
) -> Result<T, RunSupervisorError> {
    match reservation.release() {
        Ok(()) => Err(primary),
        Err(release) => Err(RunSupervisorError::ReservationRelease {
            worker: reservation.worker_id().clone(),
            pid: failed_pid(&primary),
            primary: Box::new(primary),
            detail: release.to_string(),
        }),
    }
}

fn reserved_request(reservation: &Reservation, invocation: AgentRuntimeInvocation) -> RunRequest {
    let worker_id = reservation.worker_id().clone();
    let repository = reservation.repository();
    RunRequest::new(
        reservation.agent_runtime(),
        invocation.with_session_directory(repository.root().join(".jj/pool").join("pi-sessions")),
        crate::workspace::worker_path(repository.root(), &worker_id),
        repository
            .root()
            .join(".jj/pool")
            .join(format!("{worker_id}.log")),
    )
}

fn failed_pid(error: &RunSupervisorError) -> Option<u32> {
    match error {
        RunSupervisorError::PersistPid { pid, .. }
        | RunSupervisorError::PersistPidMissing { pid, .. } => Some(*pid),
        RunSupervisorError::PersistPidCleanup { primary, .. }
        | RunSupervisorError::ReservationRelease { primary, .. } => failed_pid(primary),
        _ => None,
    }
}

fn persist_pid_failure(
    reservation: &Reservation,
    background: BackgroundRun,
    primary: RunSupervisorError,
) -> Result<BackgroundRun, RunSupervisorError> {
    let primary = match cleanup_untracked_run(background) {
        Ok(()) => primary,
        Err(source) => RunSupervisorError::PersistPidCleanup {
            worker: reservation.worker_id().clone(),
            primary: Box::new(primary),
            source,
        },
    };
    release_after_failure(reservation, primary)
}

fn cleanup_untracked_run(mut background: BackgroundRun) -> io::Result<()> {
    let pid = i32::try_from(background.pid())
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| io::Error::other("background Run PID does not fit a Unix process ID"))?;
    let termination = terminate_process_group(pid, PROCESS_GROUP_GRACE);
    background.child.wait()?;
    match termination {
        Ok(()) => Ok(()),
        Err(primary) => match test_kill_process_group(pid) {
            Err(error) if error == rustix::io::Errno::SRCH => Ok(()),
            _ => Err(primary),
        },
    }
}

/// Returns the process group for `pid` when at least one of its members is
/// still present, so a Reset can report whether it actually signaled a Run.
fn live_process_group(pid: u32) -> Option<Pid> {
    let pid = i32::try_from(pid).ok().and_then(Pid::from_raw)?;
    test_kill_process_group(pid).is_ok().then_some(pid)
}

fn terminate_process_group(pid: Pid, grace: Duration) -> io::Result<()> {
    match kill_process_group(pid, Signal::TERM) {
        Ok(()) => {}
        Err(error) if error == rustix::io::Errno::SRCH => return Ok(()),
        Err(error) => {
            return Err(io::Error::new(
                error.kind(),
                format!("cannot send TERM to process group {pid}: {error}"),
            ));
        }
    }

    if wait_for_process_group_exit(pid, grace)? {
        return Ok(());
    }

    match kill_process_group(pid, Signal::KILL) {
        Ok(()) => {}
        Err(error) if error == rustix::io::Errno::SRCH => return Ok(()),
        Err(error) => {
            return Err(io::Error::new(
                error.kind(),
                format!("cannot send KILL to process group {pid}: {error}"),
            ));
        }
    }
    if wait_for_forced_process_group_exit(pid, PROCESS_GROUP_FORCE_TIMEOUT)? {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "process group did not exit after forced termination",
        ))
    }
}

fn wait_for_process_group_exit(pid: Pid, timeout: Duration) -> io::Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        match test_kill_process_group(pid) {
            Ok(()) => {}
            Err(error) if error == rustix::io::Errno::SRCH => return Ok(true),
            // macOS can report EPERM for a just-signaled group during the
            // grace window; treat it as live and continue to the forced path.
            Err(error) if error == rustix::io::Errno::PERM => {}
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("cannot probe process group {pid}: {error}"),
                ));
            }
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(PROCESS_GROUP_POLL);
    }
}

fn wait_for_forced_process_group_exit(pid: Pid, timeout: Duration) -> io::Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        match test_kill_process_group(pid) {
            Ok(()) => {}
            Err(error) if error == rustix::io::Errno::SRCH || error == rustix::io::Errno::PERM => {
                return Ok(true);
            }
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("cannot probe forced process group {pid}: {error}"),
                ));
            }
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(PROCESS_GROUP_POLL);
    }
}

fn spawn_output_forwarder<R, W>(
    reader: R,
    terminal: W,
    log: Arc<Mutex<File>>,
    stream: &'static str,
) -> JoinHandle<Result<(), RunSupervisorError>>
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    thread::spawn(move || copy_output(reader, terminal, log, stream))
}

fn join_output_forwarder(
    forwarder: JoinHandle<Result<(), RunSupervisorError>>,
    stream: &'static str,
) -> Result<(), RunSupervisorError> {
    match forwarder.join() {
        Ok(result) => result,
        Err(_) => Err(RunSupervisorError::Forward {
            stream,
            source: io::Error::other("output forwarding thread panicked"),
        }),
    }
}

fn copy_output<R, W>(
    mut reader: R,
    mut terminal: W,
    log: Arc<Mutex<File>>,
    stream: &'static str,
) -> Result<(), RunSupervisorError>
where
    R: Read,
    W: Write,
{
    let mut buffer = [0_u8; 8192];
    let mut terminal_error = None;
    let mut log_error = None;
    let mut read_error = None;

    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(source) => {
                read_error = Some(source);
                break;
            }
        };

        if terminal_error.is_none()
            && let Err(source) = terminal.write_all(&buffer[..count])
        {
            terminal_error = Some(source);
        }
        if terminal_error.is_none()
            && let Err(source) = terminal.flush()
        {
            terminal_error = Some(source);
        }
        if log_error.is_none() {
            match log.lock() {
                Ok(mut log) => {
                    if let Err(source) = log.write_all(&buffer[..count]) {
                        log_error = Some(source);
                    }
                }
                Err(_) => {
                    log_error = Some(io::Error::other("foreground Run log lock poisoned"));
                }
            }
        }
    }

    if let Some(source) = terminal_error {
        return Err(RunSupervisorError::Forward { stream, source });
    }
    if let Some(source) = log_error {
        return Err(RunSupervisorError::Forward { stream, source });
    }
    if let Some(source) = read_error {
        return Err(RunSupervisorError::Forward { stream, source });
    }
    Ok(())
}

impl fmt::Display for AgentRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AgentRuntime {
    pub(crate) fn parse(value: &WireAgent) -> Option<Self> {
        match value.as_str() {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "pi" => Some(Self::Pi),
            _ => None,
        }
    }

    /// Selects the configured runtime, defaulting legacy pools to Claude.
    pub fn from_configured(value: Option<&WireAgent>) -> Result<Self, String> {
        let configured = value.map_or("", WireAgent::as_str).trim();
        if configured.is_empty() {
            return Ok(Self::Claude);
        }
        match configured.to_ascii_lowercase().as_str() {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "pi" => Ok(Self::Pi),
            _ => Err(configured.to_owned()),
        }
    }

    /// Returns the compatible persisted spelling for this runtime.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Pi => "pi",
        }
    }

    /// Builds the provider command for a typed invocation.
    ///
    /// The returned command executes the provider directly and never routes
    /// caller-provided prompt text through a shell.
    pub fn command(
        self,
        invocation: &AgentRuntimeInvocation,
        capabilities: AgentRuntimeCapabilities,
    ) -> Result<Command, AgentRuntimeCommandError> {
        match self {
            Self::Claude => Ok(claude_command(invocation, capabilities)),
            Self::Codex => Ok(codex_command(invocation, capabilities)),
            Self::Pi => pi_command(invocation),
        }
    }

    pub(crate) fn preflight_dispatch(
        self,
        model: Option<&AgentModel>,
        workspace: &Path,
    ) -> Result<(), AgentRuntimePreflightError> {
        if self == Self::Pi {
            let model = model.filter(|model| !model.model().is_empty()).ok_or(
                AgentRuntimePreflightError::MissingModel {
                    runtime: AgentRuntime::Pi,
                },
            )?;
            if model.provider().is_none_or(str::is_empty) {
                return Err(AgentRuntimePreflightError::MissingModelProvider {
                    runtime: AgentRuntime::Pi,
                });
            }
            PiDispatchProfile::load()?.preflight(workspace)?;
        }
        Ok(())
    }

    /// Probes this runtime in `workspace`, requiring its executable to start.
    pub fn probe(
        self,
        workspace: impl AsRef<Path>,
    ) -> Result<AgentRuntimeCapabilities, AgentRuntimeProbeError> {
        if self == Self::Pi {
            return probe_pi(workspace.as_ref());
        }
        let (arguments, capability) = match self {
            Self::Claude => (["--help"].as_slice(), Capability::ForwardSubagentText),
            Self::Codex => (["features", "list"].as_slice(), Capability::MultiAgent),
            Self::Pi => unreachable!("Pi probes through probe_pi"),
        };
        let output = Command::new(self.as_str())
            .args(arguments)
            .current_dir(workspace)
            .output()
            .map_err(|source| {
                if source.kind() == io::ErrorKind::NotFound {
                    AgentRuntimeProbeError::ExecutableNotFound { runtime: self }
                } else {
                    AgentRuntimeProbeError::Spawn {
                        runtime: self,
                        source,
                    }
                }
            })?;

        if !output.status.success() {
            return Ok(AgentRuntimeCapabilities::default());
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let supported = match capability {
            Capability::ForwardSubagentText => stdout.contains("--forward-subagent-text"),
            Capability::MultiAgent => stdout
                .lines()
                .any(|line| line.split_whitespace().next() == Some("multi_agent")),
        };
        Ok(AgentRuntimeCapabilities::from(capability, supported))
    }
}

#[derive(Debug, Deserialize)]
struct PiPackageManifest {
    name: String,
    version: String,
}

#[derive(Debug)]
struct PiDispatchProfile {
    package: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PiProfileProbe {
    all_tools: Vec<PiProfileTool>,
    active_tools: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PiProfileTool {
    name: String,
    parameters: serde_json::Value,
}

impl PiDispatchProfile {
    fn package_path() -> Result<PathBuf, AgentRuntimePreflightError> {
        let agent_directory = env::var_os("PI_CODING_AGENT_DIR")
            .filter(|directory| !directory.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("HOME")
                    .filter(|home| !home.is_empty())
                    .map(|home| PathBuf::from(home).join(".pi/agent"))
            })
            .ok_or(AgentRuntimePreflightError::MissingPiAgentDirectory)?;
        Ok(agent_directory
            .join("npm/node_modules")
            .join(PI_MCP_ADAPTER_NAME))
    }

    fn load() -> Result<Self, AgentRuntimePreflightError> {
        let package = Self::package_path()?;
        let manifest_path = package.join("package.json");
        let manifest = fs::read(&manifest_path).map_err(|source| {
            if source.kind() == io::ErrorKind::NotFound {
                AgentRuntimePreflightError::MissingPiMcpAdapter {
                    version: PI_MCP_ADAPTER_VERSION,
                }
            } else {
                AgentRuntimePreflightError::ReadPiMcpAdapterManifest { source }
            }
        })?;
        let manifest: PiPackageManifest = serde_json::from_slice(&manifest).map_err(|source| {
            AgentRuntimePreflightError::MalformedPiMcpAdapterManifest { source }
        })?;
        if manifest.name != PI_MCP_ADAPTER_NAME || manifest.version != PI_MCP_ADAPTER_VERSION {
            return Err(AgentRuntimePreflightError::UnsupportedPiMcpAdapter {
                found_name: manifest.name,
                found_version: manifest.version,
                required_version: PI_MCP_ADAPTER_VERSION,
            });
        }
        if !package.join(PI_MCP_ADAPTER_ENTRY).is_file() {
            return Err(AgentRuntimePreflightError::MissingPiMcpAdapter {
                version: PI_MCP_ADAPTER_VERSION,
            });
        }
        Ok(Self { package })
    }

    fn preflight(&self, workspace: &Path) -> Result<(), AgentRuntimePreflightError> {
        let mut probe_extension = tempfile::NamedTempFile::new()
            .map_err(|source| AgentRuntimePreflightError::PreparePiProfileProbe { source })?;
        probe_extension
            .write_all(PI_PROFILE_PROBE_EXTENSION.as_bytes())
            .map_err(|source| AgentRuntimePreflightError::PreparePiProfileProbe { source })?;
        let probe_output = tempfile::NamedTempFile::new()
            .map_err(|source| AgentRuntimePreflightError::PreparePiProfileProbe { source })?;
        let mut command = Command::new(AgentRuntime::Pi.as_str());
        command.args([
            "--mode",
            "rpc",
            "--no-session",
            "--no-extensions",
            "--extension",
        ]);
        command.arg(self.package.join(PI_MCP_ADAPTER_ENTRY));
        command.arg("--extension").arg(probe_extension.path());
        command.args([
            "--no-skills",
            "--no-prompt-templates",
            "--no-themes",
            "--no-context-files",
            "--no-approve",
            "--tools",
            PI_DISPATCH_TOOLS,
        ]);
        command
            .current_dir(workspace)
            .env(PI_PROFILE_PROBE_OUTPUT_ENV, probe_output.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let mut child = command
            .spawn()
            .map_err(|source| AgentRuntimePreflightError::StartPiProfileProbe { source })?;
        let deadline = Instant::now() + PI_PROFILE_PROBE_TIMEOUT;
        let status = loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|source| AgentRuntimePreflightError::WaitForPiProfileProbe { source })?
            {
                break status;
            }
            if Instant::now() >= deadline {
                terminate_profile_probe(&mut child);
                return Err(AgentRuntimePreflightError::PiProfileProbeTimeout);
            }
            thread::sleep(PROCESS_GROUP_POLL);
        };
        if !status.success() {
            return Err(AgentRuntimePreflightError::PiProfileProbeFailed {
                status: status.code(),
            });
        }
        let output = fs::read(probe_output.path())
            .map_err(|source| AgentRuntimePreflightError::ReadPiProfileProbe { source })?;
        let probe: PiProfileProbe = serde_json::from_slice(&output)
            .map_err(|source| AgentRuntimePreflightError::MalformedPiProfileProbe { source })?;
        for required in PI_LINEAR_TOOLS {
            if !probe.all_tools.iter().any(|tool| tool.name == required) {
                return Err(AgentRuntimePreflightError::MissingPiLinearTool { tool: required });
            }
            if !probe.active_tools.iter().any(|tool| tool == required) {
                return Err(AgentRuntimePreflightError::InactivePiLinearTool { tool: required });
            }
        }
        let get_issue = profile_tool(&probe, "linear_get_issue");
        require_profile_property(get_issue, "id")?;
        let update_issue = profile_tool(&probe, "linear_update_issue");
        require_profile_property(update_issue, "id")?;
        require_profile_property(update_issue, "assignee")?;
        if !has_profile_property(update_issue, "status")
            && !has_profile_property(update_issue, "state")
        {
            return Err(AgentRuntimePreflightError::IncompatiblePiLinearTool {
                tool: "linear_update_issue",
                requirement: "status or state",
            });
        }
        let create_comment = profile_tool(&probe, "linear_create_comment");
        require_profile_property(create_comment, "issueId")?;
        require_profile_property(create_comment, "body")?;
        Ok(())
    }
}

fn profile_tool<'a>(probe: &'a PiProfileProbe, name: &'static str) -> &'a PiProfileTool {
    probe
        .all_tools
        .iter()
        .find(|tool| tool.name == name)
        .expect("required Pi profile tool was checked before schema validation")
}

fn require_profile_property(
    tool: &PiProfileTool,
    property: &'static str,
) -> Result<(), AgentRuntimePreflightError> {
    if has_profile_property(tool, property) {
        Ok(())
    } else {
        Err(AgentRuntimePreflightError::IncompatiblePiLinearTool {
            tool: match tool.name.as_str() {
                "linear_get_issue" => "linear_get_issue",
                "linear_update_issue" => "linear_update_issue",
                "linear_create_comment" => "linear_create_comment",
                _ => unreachable!("only required Pi profile tools are validated"),
            },
            requirement: property,
        })
    }
}

fn has_profile_property(tool: &PiProfileTool, property: &str) -> bool {
    tool.parameters
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|properties| properties.contains_key(property))
}

fn terminate_profile_probe(child: &mut Child) {
    if let Some(pid) = i32::try_from(child.id()).ok().and_then(Pid::from_raw) {
        let _ = terminate_process_group(pid, PROCESS_GROUP_GRACE);
    } else {
        let _ = child.kill();
        let _ = child.wait();
    }
}

const PI_WORKER_TOOLS: &str = "read,bash,edit,write,grep,find,ls";
const PI_DISPATCH_TOOLS: &str =
    "read,bash,edit,write,grep,find,ls,linear_get_issue,linear_update_issue,linear_create_comment";
const PI_REQUIRED_FLAGS: &[&str] = &[
    "--mode",
    "--provider",
    "--model",
    "--session",
    "--session-dir",
    "--system-prompt",
    "--name",
    "--tools",
    "--no-extensions",
    "--no-skills",
    "--no-prompt-templates",
    "--no-themes",
    "--no-context-files",
    "--no-approve",
];

fn claude_command(
    invocation: &AgentRuntimeInvocation,
    capabilities: AgentRuntimeCapabilities,
) -> Command {
    let (system_prompt, prompt) = invocation.session_prompts();
    let mut command = Command::new(AgentRuntime::Claude.as_str());
    command.arg("-p");
    if let Some(model) = invocation
        .model
        .as_ref()
        .map(AgentModel::model)
        .filter(|model| !model.is_empty())
    {
        command.args(["--model", model]);
    }
    if let Some(max_budget_usd) = invocation.max_budget_usd {
        command.args(["--max-budget-usd", &max_budget_usd.to_string()]);
    }
    if let Some(session_id) = invocation
        .session_id
        .as_deref()
        .filter(|session_id| !session_id.is_empty())
    {
        command.args(["--resume", session_id, "--fork-session"]);
    }
    command.args(["--output-format", "stream-json", "--verbose"]);
    if capabilities.forward_subagent_text() {
        command.arg("--forward-subagent-text");
    }
    command.args(["--settings", r#"{"permissions":{"defaultMode":"auto"}}"#]);
    if let Some(name) = invocation.name.as_deref().filter(|name| !name.is_empty()) {
        command.args(["--name", name]);
    }
    if invocation
        .session_id
        .as_deref()
        .filter(|session_id| !session_id.is_empty())
        .is_none()
        && let Some(system_prompt) = system_prompt.as_deref()
    {
        command.args(["--append-system-prompt", system_prompt]);
    }
    command.arg(&prompt);
    command
}

fn codex_command(
    invocation: &AgentRuntimeInvocation,
    capabilities: AgentRuntimeCapabilities,
) -> Command {
    let (system_prompt, prompt) = invocation.session_prompts();
    let mut command = Command::new(AgentRuntime::Codex.as_str());
    command.args([
        "--sandbox",
        "workspace-write",
        "--ask-for-approval",
        "never",
    ]);
    if let Some(model) = invocation
        .model
        .as_ref()
        .map(AgentModel::model)
        .filter(|model| !model.is_empty())
    {
        command.args(["--model", model]);
    }
    if capabilities.multi_agent() {
        command.args(["--enable", "multi_agent"]);
    }
    command.arg("exec");
    if let Some(session_id) = invocation
        .session_id
        .as_deref()
        .filter(|session_id| !session_id.is_empty())
    {
        command.args(["resume", "--json", "--skip-git-repo-check", session_id]);
        command.arg(&prompt);
    } else {
        command.args(["--json", "--skip-git-repo-check"]);
        let prompt = match system_prompt.as_deref() {
            Some(system_prompt) => format!("{system_prompt}\n\n{prompt}"),
            None => prompt,
        };
        command.arg(prompt);
    }
    command
}

fn pi_command(invocation: &AgentRuntimeInvocation) -> Result<Command, AgentRuntimeCommandError> {
    if invocation.max_budget_usd.is_some() {
        return Err(AgentRuntimeCommandError::UnsupportedBudget {
            runtime: AgentRuntime::Pi,
        });
    }
    let (provider, model) = pi_provider_and_model(invocation.model.as_ref())?;
    let session_directory = required_pi_session_directory(invocation.session_directory.as_deref())?;
    let (system_prompt, prompt) = invocation.session_prompts();
    let mut command = Command::new(AgentRuntime::Pi.as_str());
    command.args([
        "--mode",
        "json",
        "--provider",
        provider,
        "--model",
        model,
        "--session-dir",
    ]);
    command.arg(session_directory);
    if invocation.direct_dispatch_profile {
        let package = PiDispatchProfile::package_path().map_err(|error| {
            AgentRuntimeCommandError::InvalidDispatchProfile {
                detail: error.to_string(),
            }
        })?;
        add_pi_worker_policy(&mut command, Some(&package.join(PI_MCP_ADAPTER_ENTRY)));
    } else {
        add_pi_worker_policy(&mut command, None);
    }
    if let Some(name) = invocation.name.as_deref().filter(|name| !name.is_empty()) {
        command.args(["--name", name]);
    }
    if let Some(session_id) = invocation
        .session_id
        .as_deref()
        .filter(|session_id| !session_id.is_empty())
    {
        command.args(["--session", session_id]);
    } else if let Some(system_prompt) = system_prompt.as_deref() {
        command.args(["--system-prompt", system_prompt]);
    }
    command.arg(&prompt);
    Ok(command)
}

pub(crate) fn pi_interactive_command(
    model: Option<&AgentModel>,
    session_directory: &Path,
    session_id: Option<&str>,
) -> Result<Command, AgentRuntimeCommandError> {
    let (provider, model) = pi_provider_and_model(model)?;
    let session_directory = required_pi_session_directory(Some(session_directory))?;
    let mut command = Command::new(AgentRuntime::Pi.as_str());
    command.args(["--provider", provider, "--model", model, "--session-dir"]);
    command.arg(session_directory);
    add_pi_worker_policy(&mut command, None);
    if let Some(session_id) = session_id.filter(|session_id| !session_id.is_empty()) {
        command.args(["--session", session_id]);
    }
    Ok(command)
}

fn pi_provider_and_model(
    model: Option<&AgentModel>,
) -> Result<(&str, &str), AgentRuntimeCommandError> {
    let model = model
        .filter(|model| !model.model().trim().is_empty())
        .ok_or(AgentRuntimeCommandError::MissingModel {
            runtime: AgentRuntime::Pi,
        })?;
    let provider = model
        .provider()
        .filter(|provider| !provider.trim().is_empty())
        .ok_or(AgentRuntimeCommandError::MissingProvider {
            runtime: AgentRuntime::Pi,
        })?;
    Ok((provider, model.model()))
}

fn required_pi_session_directory(
    session_directory: Option<&Path>,
) -> Result<&Path, AgentRuntimeCommandError> {
    session_directory
        .filter(|directory| !directory.as_os_str().is_empty())
        .ok_or(AgentRuntimeCommandError::MissingSessionDirectory)
}

fn add_pi_worker_policy(command: &mut Command, dispatch_package: Option<&Path>) {
    command.arg("--no-extensions");
    if let Some(package) = dispatch_package {
        command.arg("--extension").arg(package);
    }
    command.args([
        "--no-skills",
        "--no-prompt-templates",
        "--no-themes",
        "--no-context-files",
        "--no-approve",
        "--tools",
        dispatch_package.map_or(PI_WORKER_TOOLS, |_| PI_DISPATCH_TOOLS),
    ]);
}

fn probe_pi(workspace: &Path) -> Result<AgentRuntimeCapabilities, AgentRuntimeProbeError> {
    let version = run_pi_probe(["--version"].as_slice(), workspace, "version")?;
    let version_text = String::from_utf8_lossy(&version.stdout).trim().to_owned();
    let mut segments = version_text.split('.');
    let valid_version = segments.next() == Some("0")
        && segments.next() == Some("84")
        && segments
            .next()
            .is_some_and(|patch| patch.parse::<u64>().is_ok());
    if !valid_version {
        return Err(AgentRuntimeProbeError::MalformedCapabilities {
            runtime: AgentRuntime::Pi,
            detail: format!("unsupported version output {version_text:?}"),
        });
    }
    let help = run_pi_probe(["--help"].as_slice(), workspace, "help")?;
    let help_text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&help.stdout),
        String::from_utf8_lossy(&help.stderr)
    );
    if let Some(flag) = PI_REQUIRED_FLAGS
        .iter()
        .find(|flag| !help_text.contains(**flag))
    {
        return Err(AgentRuntimeProbeError::Unsupported {
            runtime: AgentRuntime::Pi,
            capability: (*flag).to_owned(),
        });
    }
    Ok(AgentRuntimeCapabilities::default())
}

fn run_pi_probe(
    arguments: &[&str],
    workspace: &Path,
    operation: &'static str,
) -> Result<std::process::Output, AgentRuntimeProbeError> {
    let output = Command::new(AgentRuntime::Pi.as_str())
        .args(arguments)
        .current_dir(workspace)
        .output()
        .map_err(|source| {
            if source.kind() == io::ErrorKind::NotFound {
                AgentRuntimeProbeError::ExecutableNotFound {
                    runtime: AgentRuntime::Pi,
                }
            } else {
                AgentRuntimeProbeError::Spawn {
                    runtime: AgentRuntime::Pi,
                    source,
                }
            }
        })?;
    if output.status.success() {
        return Ok(output);
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(AgentRuntimeProbeError::Failed {
        runtime: AgentRuntime::Pi,
        operation,
        status: output.status.code(),
        detail,
    })
}

#[derive(Debug, Clone, Copy)]
enum Capability {
    MultiAgent,
    ForwardSubagentText,
}

/// Optional capabilities discovered from an Agent Runtime executable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgentRuntimeCapabilities {
    multi_agent: bool,
    forward_subagent_text: bool,
}

impl AgentRuntimeCapabilities {
    /// Creates capabilities supplied by a runtime probe or a caller-owned adapter.
    pub const fn new(multi_agent: bool, forward_subagent_text: bool) -> Self {
        Self {
            multi_agent,
            forward_subagent_text,
        }
    }

    fn from(capability: Capability, supported: bool) -> Self {
        match capability {
            Capability::MultiAgent => Self {
                multi_agent: supported,
                ..Self::default()
            },
            Capability::ForwardSubagentText => Self {
                forward_subagent_text: supported,
                ..Self::default()
            },
        }
    }

    /// Returns whether Codex exposes the multi-agent feature.
    pub fn multi_agent(self) -> bool {
        self.multi_agent
    }

    /// Returns whether Claude Code accepts forwarded subagent text.
    pub fn forward_subagent_text(self) -> bool {
        self.forward_subagent_text
    }
}

/// Errors that prevent a typed Agent Runtime command from being built.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AgentRuntimeCommandError {
    /// Pi requires a model selection.
    #[error("{runtime} command requires a model")]
    MissingModel { runtime: AgentRuntime },
    /// Pi requires the provider portion of its model selection.
    #[error("{runtime} command requires a model provider")]
    MissingProvider { runtime: AgentRuntime },
    /// Pi runs must be isolated from the user's global session directory.
    #[error("Pi command requires an explicit session directory")]
    MissingSessionDirectory,
    /// Pi has no native aggregate budget command flag.
    #[error("{runtime} does not support an aggregate budget override")]
    UnsupportedBudget { runtime: AgentRuntime },
    /// Pi's explicit Direct Dispatch profile path could not be resolved.
    #[error("invalid Direct Dispatch runtime profile: {detail}")]
    InvalidDispatchProfile { detail: String },
}

/// Errors that prevent the selected runtime profile from satisfying Direct Dispatch.
#[derive(Debug, Error)]
pub enum AgentRuntimePreflightError {
    /// The selected runtime requires an explicit model for Direct Dispatch.
    #[error("{runtime} command requires a model")]
    MissingModel { runtime: AgentRuntime },
    /// The selected runtime requires a provider-qualified model for Direct Dispatch.
    #[error("{runtime} command requires a model provider")]
    MissingModelProvider { runtime: AgentRuntime },
    /// Pi's configuration root could not be resolved without guessing.
    #[error("Pi Direct Dispatch requires PI_CODING_AGENT_DIR or HOME")]
    MissingPiAgentDirectory,
    /// The pinned Pi MCP adapter is not installed in Pi's package directory.
    #[error("Pi Direct Dispatch requires pi-mcp-adapter {version}")]
    MissingPiMcpAdapter { version: &'static str },
    /// The installed package manifest could not be read.
    #[error("cannot read the Pi Direct Dispatch adapter manifest: {source}")]
    ReadPiMcpAdapterManifest {
        #[source]
        source: io::Error,
    },
    /// The installed package manifest is not valid JSON.
    #[error("the Pi Direct Dispatch adapter manifest is malformed: {source}")]
    MalformedPiMcpAdapterManifest {
        #[source]
        source: serde_json::Error,
    },
    /// The installed package does not match the pinned profile contract.
    #[error(
        "Pi Direct Dispatch requires pi-mcp-adapter {required_version}, found {found_name} {found_version}"
    )]
    UnsupportedPiMcpAdapter {
        found_name: String,
        found_version: String,
        required_version: &'static str,
    },
    /// The private extension used to inspect Pi tools could not be prepared.
    #[error("cannot prepare the Pi Direct Dispatch profile probe: {source}")]
    PreparePiProfileProbe {
        #[source]
        source: io::Error,
    },
    /// Pi could not start the isolated profile probe.
    #[error("cannot start the Pi Direct Dispatch profile probe: {source}")]
    StartPiProfileProbe {
        #[source]
        source: io::Error,
    },
    /// Pi's isolated profile probe could not be observed to completion.
    #[error("cannot wait for the Pi Direct Dispatch profile probe: {source}")]
    WaitForPiProfileProbe {
        #[source]
        source: io::Error,
    },
    /// Pi's isolated profile probe exceeded its fixed deadline.
    #[error("Pi Direct Dispatch profile probe timed out after 10 seconds")]
    PiProfileProbeTimeout,
    /// Pi rejected the explicit profile without exposing its private diagnostics.
    #[error("Pi Direct Dispatch profile probe failed{status}", status = status.map_or(String::new(), |status| format!(" with status {status}")))]
    PiProfileProbeFailed { status: Option<i32> },
    /// The isolated profile probe result could not be read.
    #[error("cannot read the Pi Direct Dispatch profile probe result: {source}")]
    ReadPiProfileProbe {
        #[source]
        source: io::Error,
    },
    /// The isolated profile probe did not return its versioned tool metadata.
    #[error("the Pi Direct Dispatch profile probe returned malformed data: {source}")]
    MalformedPiProfileProbe {
        #[source]
        source: serde_json::Error,
    },
    /// The configured adapter did not register one required direct Linear tool.
    #[error("Pi Direct Dispatch requires direct tool {tool}")]
    MissingPiLinearTool { tool: &'static str },
    /// A required direct Linear tool was registered but not activated.
    #[error("Pi Direct Dispatch requires active direct tool {tool}")]
    InactivePiLinearTool { tool: &'static str },
    /// A direct Linear tool cannot satisfy the fixed delivery contract.
    #[error("Pi Direct Dispatch requires direct tool {tool} schema field {requirement}")]
    IncompatiblePiLinearTool {
        tool: &'static str,
        requirement: &'static str,
    },
}

/// Errors that prevent an Agent Runtime capability probe from starting.
#[derive(Debug, Error)]
pub enum AgentRuntimeProbeError {
    /// The selected runtime executable is not available in `PATH`.
    #[error("{runtime} executable not found in PATH")]
    ExecutableNotFound { runtime: AgentRuntime },
    /// The selected runtime executable could not be started.
    #[error("cannot start {runtime} capability probe: {source}")]
    Spawn {
        runtime: AgentRuntime,
        #[source]
        source: io::Error,
    },
    /// The selected runtime rejected a required capability probe.
    #[error("{runtime} {operation} capability probe failed{}{}", status.map_or(String::new(), |status| format!(" with status {status}")), if detail.is_empty() { String::new() } else { format!(": {detail}") })]
    Failed {
        /// Runtime being probed.
        runtime: AgentRuntime,
        /// Probe operation that failed.
        operation: &'static str,
        /// Process status when one was available.
        status: Option<i32>,
        /// Sanitized process diagnostic.
        detail: String,
    },
    /// The runtime returned malformed capability information.
    #[error("{runtime} capability probe returned malformed data: {detail}")]
    MalformedCapabilities {
        /// Runtime being probed.
        runtime: AgentRuntime,
        /// Decoding detail.
        detail: String,
    },
    /// A required runtime capability is absent.
    #[error("{runtime} does not support required capability {capability}")]
    Unsupported {
        /// Runtime being probed.
        runtime: AgentRuntime,
        /// Missing flag or capability name.
        capability: String,
    },
}
