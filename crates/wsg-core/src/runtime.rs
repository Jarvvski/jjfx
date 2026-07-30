//! Agent Runtime identity and execution capability probing.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};

use thiserror::Error;

use crate::pool::RunClearing;
use crate::{
    Repository, Reservation, StateError, StateRevision, WireAgent, WorkerId, WorkerPoolError,
    WorkerState,
};

const PROCESS_GROUP_GRACE: Duration = Duration::from_secs(1);
const PROCESS_GROUP_POLL: Duration = Duration::from_millis(10);
const PROCESS_GROUP_FORCE_TIMEOUT: Duration = Duration::from_secs(1);

/// The Agent Runtime recorded for a Worker and selected for a Run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRuntime {
    /// Anthropic's Claude Code runtime.
    Claude,
    /// OpenAI's Codex runtime.
    Codex,
}

/// Typed inputs for one Agent Runtime invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRuntimeInvocation {
    prompt: String,
    model: Option<String>,
    session_id: Option<String>,
    name: Option<String>,
    system_prompt: Option<String>,
}

impl AgentRuntimeInvocation {
    /// Creates a fresh invocation with the required workload prompt.
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            model: None,
            session_id: None,
            name: None,
            system_prompt: None,
        }
    }

    /// Adds a model override to the invocation.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Adds an Agent Session to resume.
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Adds a display name for a fresh Claude invocation.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Adds a system prompt for a fresh invocation.
    pub fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(system_prompt.into());
        self
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
        let mut command = request.runtime.command(&request.invocation, capabilities);
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
        reservation: &Reservation,
        invocation: AgentRuntimeInvocation,
    ) -> Result<RunOutcome, RunSupervisorError> {
        let request = reserved_request(reservation, invocation);
        let outcome = match self.run_foreground(&request) {
            Ok(outcome) => outcome,
            Err(error) => return release_after_failure(reservation, error),
        };
        RunCompletion::new(reservation.clone(), reservation.worker_revision()).finalize(outcome)?;
        Ok(outcome)
    }

    /// Starts a reserved Run and persists its process identifier before success.
    pub fn run_reserved_background(
        &self,
        reservation: &Reservation,
        invocation: AgentRuntimeInvocation,
    ) -> Result<BackgroundRun, RunSupervisorError> {
        let worker_id = reservation.worker_id().clone();
        let request = reserved_request(reservation, invocation);
        let mut background = match self.run_background(&request) {
            Ok(background) => background,
            Err(error) => return release_after_failure(reservation, error),
        };
        let pid = background.pid();
        match reservation.persist_pid(pid) {
            Ok(crate::pool::PidPersistence::Persisted(revision)) => {
                background.completion = Some(RunCompletion::new(reservation.clone(), revision));
                Ok(background)
            }
            Ok(crate::pool::PidPersistence::Missing) => persist_pid_failure(
                reservation,
                background,
                RunSupervisorError::PersistPidMissing {
                    worker: worker_id,
                    pid,
                },
            ),
            Ok(crate::pool::PidPersistence::Conflict) => persist_pid_failure(
                reservation,
                background,
                RunSupervisorError::PersistPidConflict { worker: worker_id },
            ),
            Err(source) => persist_pid_failure(
                reservation,
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
        let mut command = request.runtime.command(&request.invocation, capabilities);
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

    fn finalize(self, outcome: RunOutcome) -> Result<(), RunSupervisorError> {
        let worker = self.reservation.worker_id().clone();
        self.reservation
            .finalize(self.revision, outcome.exit_code())
            .map(|_| ())
            .map_err(|source| RunSupervisorError::Finalize { worker, source })
    }
}

impl BackgroundRun {
    /// Returns the process ID, which is also the Run's process-group ID.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Waits for the background Run and reaps its process.
    pub fn wait(mut self) -> Result<RunOutcome, RunSupervisorError> {
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
            completion.finalize(outcome)?;
        }
        Ok(outcome)
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
        invocation,
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
            _ => None,
        }
    }

    pub(crate) fn from_configured(value: Option<&WireAgent>) -> Result<Self, String> {
        let configured = value.map_or("", WireAgent::as_str).trim();
        if configured.is_empty() {
            return Ok(Self::Claude);
        }
        match configured.to_ascii_lowercase().as_str() {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            _ => Err(configured.to_owned()),
        }
    }

    /// Returns the compatible persisted spelling for this runtime.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
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
    ) -> Command {
        let mut command = Command::new(self.as_str());
        if self == Self::Claude {
            command.arg("-p");
            if let Some(model) = invocation
                .model
                .as_deref()
                .filter(|model| !model.is_empty())
            {
                command.args(["--model", model]);
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
                && let Some(system_prompt) = invocation
                    .system_prompt
                    .as_deref()
                    .filter(|prompt| !prompt.is_empty())
            {
                command.args(["--append-system-prompt", system_prompt]);
            }
            command.arg(&invocation.prompt);
        } else {
            command.args([
                "--sandbox",
                "workspace-write",
                "--ask-for-approval",
                "never",
            ]);
            if let Some(model) = invocation
                .model
                .as_deref()
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
                command.arg(&invocation.prompt);
            } else {
                command.args(["--json", "--skip-git-repo-check"]);
                let prompt = match invocation
                    .system_prompt
                    .as_deref()
                    .filter(|prompt| !prompt.is_empty())
                {
                    Some(system_prompt) => format!("{system_prompt}\n\n{}", invocation.prompt),
                    None => invocation.prompt.clone(),
                };
                command.arg(prompt);
            }
        }
        command
    }

    /// Probes this runtime in `workspace`, requiring its executable to start.
    pub fn probe(
        self,
        workspace: impl AsRef<Path>,
    ) -> Result<AgentRuntimeCapabilities, AgentRuntimeProbeError> {
        let (arguments, capability) = match self {
            Self::Claude => (["--help"].as_slice(), Capability::ForwardSubagentText),
            Self::Codex => (["features", "list"].as_slice(), Capability::MultiAgent),
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
}
