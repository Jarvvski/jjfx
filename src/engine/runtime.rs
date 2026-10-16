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

use rustix::process::{
    Pid, Signal, getpgid, kill_process_group, test_kill_process, test_kill_process_group,
};
use thiserror::Error;

use crate::engine::pool::RunClearing;
use crate::engine::{
    Repository, Reservation, RunResult, RunResultSource, StateError, StateRevision, WireAgent,
    WorkerId, WorkerPoolError, WorkerState,
};

const PROCESS_GROUP_GRACE: Duration = Duration::from_secs(1);
const PROCESS_GROUP_POLL: Duration = Duration::from_millis(10);
const PROCESS_GROUP_FORCE_TIMEOUT: Duration = Duration::from_secs(1);
/// The persisted spelling of the Pi Agent Runtime before it was removed.
///
/// The compatibility contract fixed `pi` as a canonical Worker runtime value,
/// so a pool or Worker that still carries it must be rejected with a clear
/// message rather than a bare parse error, and reconciled rather than left
/// stranded `busy`.
pub(crate) const REMOVED_PI_RUNTIME: &str = "pi";
const CLAUDE_DISPATCH_GUIDANCE: &str = "Linear delivery tools are available as the mcp__claude_ai_Linear__get_issue, mcp__claude_ai_Linear__update_issue, and mcp__claude_ai_Linear__create_comment MCP tools when this session has them. Use them to update status or post delivery notes; if they are unavailable, continue the implementation and note it - do not stop.";
const NEUTRAL_DISPATCH_GUIDANCE: &str = "Use your configured Linear/MCP delivery tools to update status or post delivery notes when they are available. If they are unavailable, continue the implementation and note it - do not stop.";
const DELEGATION_RULES: &str = "Delegated work is read-only.\n\n- Use in-session background tasks or subagents only for independent exploration, documentation lookup, test or log analysis, or review.\n- Explicitly tell every subagent not to edit tracked files or run jj commands.\n- Do not use detached sessions, nested delegation, or worktree or workspace creation.\n- Await all delegated work before finishing.\n- If delegation is unavailable or fails, continue the work directly.\n- The main agent alone owns tracked edits, jj operations, verification, and delivery.";

/// Whether a persisted Agent Runtime value names the removed Pi runtime.
///
/// Normalizes exactly as [`AgentRuntime::from_configured`] does, so a value it
/// would have selected as Pi is recognized here too.
pub(crate) fn is_removed_runtime(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case(REMOVED_PI_RUNTIME)
}

/// The clear, actionable message for a persisted value that names the removed
/// Pi Agent Runtime.
pub(crate) fn removed_runtime_message(value: &str) -> String {
    format!(
        "the Pi Agent Runtime ({}) was removed; choose claude, codex, or opencode with `jjfx pool profile <runtime>`",
        value.trim()
    )
}

/// The Linear delivery guidance prepended to a TicketDelivery prompt, matched to
/// the tools the selected Agent Runtime actually exposes.
fn ticket_delivery_guidance(runtime: AgentRuntime) -> &'static str {
    match runtime {
        AgentRuntime::Claude => CLAUDE_DISPATCH_GUIDANCE,
        AgentRuntime::Codex | AgentRuntime::OpenCode => NEUTRAL_DISPATCH_GUIDANCE,
    }
}

/// The Agent Runtime recorded for a Worker and selected for a Run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRuntime {
    /// Anthropic's Claude Code runtime.
    Claude,
    /// OpenAI's Codex runtime.
    Codex,
    /// OpenCode's multi-provider coding-agent runtime.
    OpenCode,
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

    /// Adds the provider a runtime needs alongside the model.
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

/// The complete provider-neutral identity and model policy for one Agent Runtime Run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRuntimeProfile {
    runtime: AgentRuntime,
    model: Option<AgentModel>,
    agent: Option<String>,
}

impl AgentRuntimeProfile {
    /// Creates a profile using provider-managed model selection.
    pub const fn new(runtime: AgentRuntime) -> Self {
        Self {
            runtime,
            model: None,
            agent: None,
        }
    }

    /// Supplies the provider-aware model selection for this profile.
    pub fn with_model(mut self, model: impl Into<AgentModel>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Selects a named provider-side agent, when the runtime supports one.
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    /// Returns the optional provider-side agent name.
    pub fn agent(&self) -> Option<&str> {
        self.agent.as_deref()
    }

    /// Reconstructs a compatible profile from persisted runtime, provider, and model fields.
    pub fn from_configured(
        runtime: Option<&WireAgent>,
        provider: Option<&str>,
        model: Option<&str>,
    ) -> Result<Self, String> {
        let runtime = AgentRuntime::from_configured(runtime)?;
        Ok(Self::from_parts(runtime, provider, model))
    }

    /// Reconstructs an optional Run profile without applying the legacy Pool default.
    pub(crate) fn from_run_state(
        runtime: Option<&WireAgent>,
        provider: Option<&str>,
        model: Option<&str>,
    ) -> Result<Option<Self>, String> {
        let Some(runtime) = runtime.filter(|runtime| !runtime.as_str().trim().is_empty()) else {
            return Ok(None);
        };
        let runtime = AgentRuntime::from_configured(Some(runtime))?;
        Ok(Some(Self::from_parts(runtime, provider, model)))
    }

    fn from_parts(runtime: AgentRuntime, provider: Option<&str>, model: Option<&str>) -> Self {
        let provider = provider.map(str::trim).filter(|value| !value.is_empty());
        let model = model.map(str::trim).filter(|value| !value.is_empty());
        let selection = match (provider, model) {
            (None, None) => None,
            (provider, model) => {
                let mut selection = AgentModel::new(model.unwrap_or_default());
                if let Some(provider) = provider {
                    selection = selection.with_provider(provider);
                }
                Some(selection)
            }
        };
        Self {
            runtime,
            model: selection,
            agent: None,
        }
    }

    /// Applies a caller model override while retaining a configured provider when omitted.
    pub fn with_model_override(mut self, model: Option<&AgentModel>) -> Self {
        let Some(model) = model else {
            return self;
        };
        let provider = model.provider().map(str::to_owned).or_else(|| {
            self.model
                .as_ref()
                .and_then(AgentModel::provider)
                .map(str::to_owned)
        });
        let mut merged = AgentModel::new(model.model());
        if let Some(provider) = provider {
            merged = merged.with_provider(provider);
        }
        self.model = Some(merged);
        self
    }

    /// Returns the selected Agent Runtime.
    pub const fn runtime(&self) -> AgentRuntime {
        self.runtime
    }

    /// Returns the optional provider-aware model selection.
    pub const fn model(&self) -> Option<&AgentModel> {
        self.model.as_ref()
    }

    pub(crate) fn provider_value(&self) -> Option<String> {
        self.model
            .as_ref()
            .and_then(AgentModel::provider)
            .map(str::to_owned)
    }

    pub(crate) fn model_value(&self) -> Option<String> {
        self.model
            .as_ref()
            .map(AgentModel::model)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum AgentRuntimeInvocationProfile {
    #[default]
    Standard,
    TicketDelivery,
}

/// Typed inputs for one Agent Runtime invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRuntimeInvocation {
    prompt: String,
    model: Option<AgentModel>,
    max_budget_usd: Option<u32>,
    session_id: Option<String>,
    name: Option<String>,
    agent: Option<String>,
    system_prompt: Option<String>,
    profile: AgentRuntimeInvocationProfile,
}

impl AgentRuntimeInvocation {
    /// Creates a fresh invocation with the required workload prompt.
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            model: None,
            max_budget_usd: None,
            session_id: None,
            name: None,
            agent: None,
            system_prompt: None,
            profile: AgentRuntimeInvocationProfile::Standard,
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

    /// Adds a display name for a fresh Agent Runtime invocation.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Selects a named provider-side agent.
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    /// Adds a system prompt for a fresh invocation.
    pub fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(system_prompt.into());
        self
    }

    pub(crate) fn with_ticket_delivery_profile(mut self) -> Self {
        self.profile = AgentRuntimeInvocationProfile::TicketDelivery;
        self
    }

    fn session_prompts(&self, runtime: AgentRuntime) -> (Option<String>, String) {
        let prompt = if self.profile == AgentRuntimeInvocationProfile::TicketDelivery {
            format!("{}\n\n{}", ticket_delivery_guidance(runtime), self.prompt)
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

    /// The prompt text this invocation delivers to `runtime`, after the
    /// delegation rules and any ticket-delivery guidance are applied.
    ///
    /// This is what a Run's session receives whether the prompt is passed to a
    /// spawned client or delivered straight to the session through the daemon
    /// API, so both paths send the same text.
    pub(crate) fn delivered_prompt(&self, runtime: AgentRuntime) -> String {
        self.session_prompts(runtime).1
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
        let agent = request.runtime.command(&request.invocation, capabilities);
        // A foreground Run owns its process group exactly like a background Run,
        // so a failed launch and a cross-process Reset can end its whole tree with
        // one group signal instead of leaking agent descendants. Owning its own
        // group moves the Run out of the terminal's foreground group, so the
        // terminal is handed to the Run's group and handed back by the wrapper.
        let handover = super::foreground::Handover::capture();
        let mut command = match handover.as_ref() {
            Some(handover) => super::foreground::wrapper_command(&agent, handover)
                .map_err(|source| RunSupervisorError::TerminalHandover { source })?,
            None => agent,
        };
        command
            .current_dir(&request.workspace)
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command
            .spawn()
            .map_err(|source| RunSupervisorError::Spawn {
                runtime: request.runtime,
                source,
            })?;
        if let Some(handover) = handover.as_ref()
            && let Err(source) = handover.give_to(child.id())
        {
            let _ = terminate_foreground_run(&mut child);
            return Err(RunSupervisorError::TerminalHandover { source });
        }
        if let Err(error) = started(child.id()) {
            let _ = terminate_foreground_run(&mut child);
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
                let _ = terminate_foreground_run(&mut child);
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

    /// Prepares an opencode Run's session before launch and records its id.
    ///
    /// A fresh opencode Run has its session created up front with the Worker
    /// Workspace as its `location.directory`, so the Run is anchored to that
    /// workspace rather than inferred from the spawned process's cwd. The id is
    /// recorded in pool-owned sidecar state so a later, different process can
    /// identify the Run's session.
    ///
    /// Everything else is returned unchanged: another runtime, or an opencode
    /// whose API is unavailable, falls back to today's process-cwd invocation.
    /// An opencode Run that drives no session also forgets the session recorded
    /// for the Worker's previous Run, so the sidecar never names a session that
    /// is not the current Run's.
    /// A Run that already carries a session id is a resume, and keeps it. A
    /// reachable opencode that anchors the session anywhere other than the
    /// Worker Workspace fails the Run rather than let it commit into the wrong
    /// worktree.
    fn prepare_reserved_session(
        &self,
        reservation: &Reservation,
        invocation: AgentRuntimeInvocation,
    ) -> Result<AgentRuntimeInvocation, RunSupervisorError> {
        if reservation.agent_runtime() != AgentRuntime::OpenCode {
            return Ok(invocation);
        }
        // A resumed session already exists; keep it, and record its id so a later
        // process can still identify the Run's session.
        if let Some(session_id) = invocation
            .session_id
            .as_deref()
            .filter(|session_id| !session_id.is_empty())
        {
            self.record_session(reservation, session_id)?;
            return Ok(invocation);
        }
        let worker = reservation.worker_id();
        let workspace =
            crate::engine::workspace::worker_path(reservation.repository().root(), worker);
        let session_id = match super::opencode_api::create_run_session(
            &crate::engine::OpenCodeApi::new(),
            &workspace,
            invocation.name.as_deref(),
        ) {
            Ok(Some(session_id)) => session_id,
            Ok(None) => {
                // The API is unreachable, so this Run drives no session. Forget
                // the previous one rather than leave its id in place: a later
                // reconcile would read that Run's outcome and finish this
                // Worker while its work is still going.
                self.forget_session(reservation)?;
                return Ok(invocation);
            }
            Err(reason) => {
                return Err(RunSupervisorError::OpenCodeSession {
                    worker: worker.clone(),
                    reason,
                });
            }
        };
        self.record_session(reservation, &session_id)?;
        Ok(invocation.with_session_id(session_id))
    }

    /// Records the session id the Worker's current Run drives in pool-owned
    /// sidecar state, so a later, different process can identify it.
    fn record_session(
        &self,
        reservation: &Reservation,
        session_id: &str,
    ) -> Result<(), RunSupervisorError> {
        let worker = reservation.worker_id();
        reservation
            .repository()
            .state_store()
            .sessions()
            .record(worker, session_id)
            .map_err(|source| RunSupervisorError::OpenCodeSession {
                worker: worker.clone(),
                reason: format!("cannot record the Run's session id: {source}"),
            })
    }

    /// Forgets the session recorded for the Worker's previous Run.
    fn forget_session(&self, reservation: &Reservation) -> Result<(), RunSupervisorError> {
        let worker = reservation.worker_id();
        reservation
            .repository()
            .state_store()
            .sessions()
            .clear(worker)
            .map_err(|source| RunSupervisorError::OpenCodeSession {
                worker: worker.clone(),
                reason: format!("cannot forget the Run's session id: {source}"),
            })
    }

    pub(crate) fn run_reserved_foreground_with_handoff(
        &self,
        reservation: Reservation,
        invocation: AgentRuntimeInvocation,
        handoff: impl FnOnce(),
    ) -> Result<CompletedRun, RunSupervisorError> {
        let worker = reservation.worker_id().clone();
        let invocation = match self.prepare_reserved_session(&reservation, invocation) {
            Ok(invocation) => invocation,
            Err(error) => return release_after_failure(&reservation, error),
        };
        let request = reserved_request(&reservation, invocation);
        let mut persisted_revision = None;
        let outcome = match self.run_foreground_started(&request, |pid| {
            let revision = match reservation.persist_pid(pid) {
                Ok(crate::engine::pool::PidPersistence::Persisted(revision)) => revision,
                Ok(crate::engine::pool::PidPersistence::Missing) => {
                    return Err(RunSupervisorError::PersistPidMissing {
                        worker: worker.clone(),
                        pid,
                    });
                }
                Ok(crate::engine::pool::PidPersistence::Conflict) => {
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
        let invocation = match self.prepare_reserved_session(&reservation, invocation) {
            Ok(invocation) => invocation,
            Err(error) => return release_after_failure(&reservation, error),
        };
        let request = reserved_request(&reservation, invocation);
        let mut background = match self.run_background(&request) {
            Ok(background) => background,
            Err(error) => return release_after_failure(&reservation, error),
        };
        let pid = background.pid();
        match reservation.persist_pid(pid) {
            Ok(crate::engine::pool::PidPersistence::Persisted(revision)) => {
                background.completion = Some(RunCompletion::new(reservation, revision));
                Ok(background)
            }
            Ok(crate::engine::pool::PidPersistence::Missing) => persist_pid_failure(
                &reservation,
                background,
                RunSupervisorError::PersistPidMissing {
                    worker: worker_id,
                    pid,
                },
            ),
            Ok(crate::engine::pool::PidPersistence::Conflict) => persist_pid_failure(
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
        let terminated_pid = self.stop_run(repository, worker, &target)?;
        match pool.clear_run(worker, target).map_err(reset)? {
            RunClearing::Cleared => Ok(RunReset::Abandoned { terminated_pid }),
            RunClearing::AlreadyIdle => Ok(RunReset::AlreadyIdle),
            RunClearing::Superseded => Ok(RunReset::Superseded),
        }
    }

    /// Ends a Run's work, returning the process group that was terminated.
    ///
    /// An opencode Run's work runs in the shared daemon, not in the process
    /// jjfx spawned, so it is stopped through its session - signalling the
    /// client would leave the work running. Every other runtime owns its work,
    /// and pid/process-group signalling is also the fallback when the daemon
    /// cannot be reached or an older opencode lacks the route.
    fn stop_run(
        &self,
        repository: &Repository,
        worker: &WorkerId,
        target: &crate::engine::pool::RunTarget,
    ) -> Result<Option<u32>, RunSupervisorError> {
        if target.runtime() == Some(AgentRuntime::OpenCode)
            && interrupt_opencode_run(repository, worker, target.ticket())
        {
            return Ok(None);
        }
        match target.pid().and_then(live_process_group_leader) {
            Some(pid) => {
                terminate_process_group(pid, PROCESS_GROUP_GRACE).map_err(|source| {
                    RunSupervisorError::ResetCleanup {
                        worker: worker.clone(),
                        pid: pid.as_raw_nonzero().get().unsigned_abs(),
                        source,
                    }
                })?;
                Ok(Some(pid.as_raw_nonzero().get().unsigned_abs()))
            }
            None => Ok(None),
        }
    }

    /// Terminates a persisted background Run without changing Worker state.
    ///
    /// Pool destruction uses this after atomically detaching membership. No
    /// state lock is held while the process group is signaled. Only an opencode
    /// Run's work lives in the shared daemon, so only its recorded session is
    /// consulted; any other runtime owns its work, and signalling its process
    /// group is the whole stop. The sidecar that names a session is recorded and
    /// cleared by opencode Runs alone, so a Worker that ran opencode and then a
    /// different runtime can still carry a stale opencode id - gating on the
    /// persisted runtime keeps that stale id from standing in for this Run.
    pub(crate) fn terminate_recorded_process(
        &self,
        repository: &Repository,
        worker: &WorkerId,
        runtime: Option<AgentRuntime>,
        pid: u32,
    ) -> io::Result<()> {
        if runtime == Some(AgentRuntime::OpenCode)
            && interrupt_opencode_run(repository, worker, None)
        {
            return Ok(());
        }
        // Only a live process-group leader is a Run to signal. A recycled PID
        // that does not lead a group has no group with that id, so this returns
        // without signalling anything.
        let Some(pid) = live_process_group_leader(pid) else {
            return Ok(());
        };
        terminate_process_group(pid, PROCESS_GROUP_GRACE)
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
            .state_store()
            .worker_log(&worker);
        let (result, source) = result_for_completion(
            &log_path,
            self.reservation.agent_runtime(),
            outcome.exit_code,
        );
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
            result_for_completion(&self.log_path, self.runtime, outcome.exit_code);
        Ok(CompletedRun {
            process: outcome,
            result,
            result_source,
        })
    }
}

fn result_for_completion(
    log_path: &Path,
    runtime: AgentRuntime,
    exit_code: Option<i32>,
) -> (RunResult, RunResultSource) {
    let (result, source) = crate::engine::run_log::result_for_finalization(log_path, runtime);
    if runtime != AgentRuntime::OpenCode {
        return (result, source);
    }
    // opencode emits no terminal event on success, so a clean exit is its own
    // proof of success. Either way the Run's cost and tokens are summed from its
    // own step events, which the provider reports per step rather than per Run.
    if exit_code == Some(0) && matches!(source, RunResultSource::Fallback { .. }) {
        return (
            crate::engine::run_log::with_opencode_totals(RunResult::succeeded(), log_path),
            RunResultSource::Provider,
        );
    }
    (
        crate::engine::run_log::with_opencode_totals(result, log_path),
        source,
    )
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
    /// The Run's opencode session could not be anchored to its Worker Workspace.
    ///
    /// Only a reachable opencode produces this: an unavailable API falls back to
    /// the process-cwd invocation instead. Failing here stops a Run that would
    /// otherwise commit into the wrong worktree.
    #[error("cannot anchor Worker {worker}'s opencode session: {reason}")]
    OpenCodeSession {
        /// The Worker whose Run could not be prepared.
        worker: WorkerId,
        /// Why the Run must not proceed.
        reason: String,
    },
    /// The terminal could not be handed to the Run's process group.
    #[error("cannot hand the terminal to the foreground Run: {source}")]
    TerminalHandover {
        /// The terminal job-control error.
        #[source]
        source: io::Error,
    },
    /// The Worker terminal state could not be persisted.
    #[error("cannot finalize Run for Worker {worker}: {source}")]
    Finalize {
        /// The Worker whose Run completed.
        worker: crate::engine::WorkerId,
        /// The Worker state repository failure.
        #[source]
        source: WorkerPoolError,
    },
    /// The Worker PID could not be loaded or persisted.
    #[error("cannot persist PID for Worker {worker}: {source}")]
    PersistPid {
        /// The Worker whose Run was launched.
        worker: crate::engine::WorkerId,
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
        worker: crate::engine::WorkerId,
        /// The process identifier that could not be recorded.
        pid: u32,
    },
    /// Another mutation replaced the reserved Worker state before PID persistence.
    #[error("cannot persist PID for Worker {worker}: Worker state changed after reservation")]
    PersistPidConflict {
        /// The Worker whose Run was launched.
        worker: crate::engine::WorkerId,
    },
    /// The untracked process group could not be cleaned up after persistence failed.
    #[error("cannot clean up untracked Run for Worker {worker}: {source}")]
    PersistPidCleanup {
        /// The Worker whose Run was launched.
        worker: crate::engine::WorkerId,
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
        worker: crate::engine::WorkerId,
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
        crate::engine::workspace::worker_path(repository.root(), &worker_id),
        repository.state_store().worker_log(&worker_id),
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
    persist_pid_failure_with(reservation, background, primary, cleanup_untracked_run)
}

/// Orders an untracked Run's cleanup against its Reservation release.
///
/// The guarantee is that a Reservation is released only after its process group
/// is actually gone. Its failure mode needs a process group to survive `SIGKILL`,
/// which cannot be arranged deterministically, so the cleanup outcome is a
/// parameter: a test injects the failure through this seam instead of arranging
/// the impossible process state.
fn persist_pid_failure_with(
    reservation: &Reservation,
    background: BackgroundRun,
    primary: RunSupervisorError,
    cleanup: impl FnOnce(BackgroundRun) -> io::Result<()>,
) -> Result<BackgroundRun, RunSupervisorError> {
    match cleanup(background) {
        // The process group is gone, so the Worker may safely be released.
        Ok(()) => release_after_failure(reservation, primary),
        // The process group may still be alive: keep the Reservation so the
        // Worker cannot be reused while an untracked agent may still be running.
        Err(source) => Err(RunSupervisorError::PersistPidCleanup {
            worker: reservation.worker_id().clone(),
            primary: Box::new(primary),
            source,
        }),
    }
}

fn cleanup_untracked_run(mut background: BackgroundRun) -> io::Result<()> {
    let pid = recorded_pid(background.pid())
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

/// Ends a foreground Run that never reached supervision: signal its process
/// group so no descendant survives, then reap the direct child.
fn terminate_foreground_run(child: &mut Child) -> io::Result<()> {
    let termination = recorded_pid(child.id()).map_or(Ok(()), |pid| {
        terminate_process_group(pid, PROCESS_GROUP_GRACE)
    });
    let reaped = child.wait().map(|_| ());
    termination.and(reaped)
}

/// Converts a recorded PID to a process-group id, or `None` when it cannot be one.
fn recorded_pid(raw: u32) -> Option<Pid> {
    i32::try_from(raw).ok().and_then(Pid::from_raw)
}

/// Identifies the opencode session a Worker's Run drives, if it can be told.
///
/// The id ticket 38 recorded for the Worker is authoritative; only when nothing
/// is recorded does this fall back to matching the Run title against the
/// daemon's active sessions. Shared by the stop path and by Worker
/// reconciliation, so the two always name the same session.
pub(crate) fn identify_run_session(
    api: &crate::engine::OpenCodeApi,
    repository: &Repository,
    worker: &WorkerId,
    ticket: Option<&str>,
) -> Option<String> {
    let recorded = repository
        .state_store()
        .sessions()
        .session(worker)
        .ok()
        .flatten();
    let title = ticket
        .filter(|ticket| !ticket.is_empty())
        .map(|ticket| crate::engine::opencode_api::run_session_title(worker, ticket));
    crate::engine::opencode_api::run_session_id(api, recorded.as_deref(), title.as_deref())
}

/// Stops an opencode Run through its daemon session, reporting whether the
/// daemon confirmed it interrupted an active execution.
///
/// Best-effort throughout: an unreachable or older opencode, an unidentified
/// session, or a session that is not executing all report `false`, so the
/// caller falls back to pid/process-group signalling rather than failing.
fn interrupt_opencode_run(
    repository: &Repository,
    worker: &WorkerId,
    ticket: Option<&str>,
) -> bool {
    let api = crate::engine::OpenCodeApi::new();
    let Some(session_id) = identify_run_session(&api, repository, worker, ticket) else {
        return false;
    };
    matches!(api.interrupt_session(&session_id), Ok(true))
}

/// Returns the process group for `pid` only when `pid` is a live process-group
/// leader.
///
/// A recorded PID is a liveness hint, not a Run identity. Requiring the PID to
/// lead its own group closes the recycled-PID hazard for a PID that does not
/// lead a group - there is no group with that id to signal - and keeps Reset and
/// Worker reconciliation on one group-based probe.
pub(crate) fn live_process_group_leader(pid: u32) -> Option<Pid> {
    let pid = recorded_pid(pid)?;
    if test_kill_process(pid).is_err() {
        return None;
    }
    (getpgid(Some(pid)).ok()? == pid).then_some(pid)
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
            "opencode" => Some(Self::OpenCode),
            _ => None,
        }
    }

    /// Selects the configured runtime, defaulting legacy pools to Claude.
    ///
    /// The error is a complete, actionable message: a removed runtime names the
    /// removal, and an unknown one names the value and the supported set.
    pub fn from_configured(value: Option<&WireAgent>) -> Result<Self, String> {
        let configured = value.map_or("", WireAgent::as_str).trim();
        if configured.is_empty() {
            return Ok(Self::Claude);
        }
        if is_removed_runtime(configured) {
            return Err(removed_runtime_message(configured));
        }
        match configured.to_ascii_lowercase().as_str() {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "opencode" => Ok(Self::OpenCode),
            _ => Err(format!(
                "invalid configured Agent Runtime {configured:?} (expected claude, codex, or opencode)"
            )),
        }
    }

    /// Returns the compatible persisted spelling for this runtime.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
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
        match self {
            Self::Claude => claude_command(invocation, capabilities),
            Self::Codex => codex_command(invocation, capabilities),
            Self::OpenCode => opencode_command(invocation),
        }
    }

    /// Probes this runtime in `workspace`, requiring its executable to start.
    pub fn probe(
        self,
        workspace: impl AsRef<Path>,
    ) -> Result<AgentRuntimeCapabilities, AgentRuntimeProbeError> {
        let (arguments, capability) = match self {
            Self::Claude => (["--help"].as_slice(), Capability::ForwardSubagentText),
            Self::Codex => (["features", "list"].as_slice(), Capability::MultiAgent),
            Self::OpenCode => (["--version"].as_slice(), Capability::ForwardSubagentText),
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

fn claude_command(
    invocation: &AgentRuntimeInvocation,
    capabilities: AgentRuntimeCapabilities,
) -> Command {
    let (system_prompt, prompt) = invocation.session_prompts(AgentRuntime::Claude);
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
    let (system_prompt, prompt) = invocation.session_prompts(AgentRuntime::Codex);
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

fn opencode_command(invocation: &AgentRuntimeInvocation) -> Command {
    let (system_prompt, prompt) = invocation.session_prompts(AgentRuntime::OpenCode);
    let mut command = Command::new(AgentRuntime::OpenCode.as_str());
    command.args(["run", "--format", "json", "--auto"]);
    if invocation.profile == AgentRuntimeInvocationProfile::TicketDelivery {
        command.args(["--agent", "build"]);
    }
    if let Some(model) = invocation
        .model
        .as_ref()
        .filter(|model| !model.model().is_empty())
    {
        let model = model.provider().map_or_else(
            || model.model().to_owned(),
            |provider| format!("{provider}/{}", model.model()),
        );
        command.args(["--model", &model]);
    }
    if let Some(session_id) = invocation
        .session_id
        .as_deref()
        .filter(|session_id| !session_id.is_empty())
    {
        command.args(["--session", session_id]);
    }
    if let Some(name) = invocation.name.as_deref().filter(|name| !name.is_empty()) {
        command.args(["--title", name]);
    }
    if let Some(agent) = invocation
        .agent
        .as_deref()
        .filter(|agent| !agent.is_empty())
    {
        command.args(["--agent", agent]);
    }
    let prompt = system_prompt.map_or(prompt.clone(), |system_prompt| {
        format!("{system_prompt}\n\n{prompt}")
    });
    command.arg(prompt);
    command
}

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

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::process::Command;

    use crate::engine::{PoolCapacity, Repository, WorkerStatus};

    /// A throwaway Jujutsu repository with one idle Worker, so the ordering test
    /// owns a real Reservation rather than a stub.
    fn reserved_worker() -> (tempfile::TempDir, Repository, crate::engine::WorkerId) {
        let temporary = tempfile::TempDir::new().expect("temporary directory");
        let status = Command::new("jj")
            .args([
                "--config",
                "signing.behavior=drop",
                "git",
                "init",
                "--no-colocate",
            ])
            .arg(temporary.path())
            .status()
            .expect("jj should be installed");
        assert!(status.success(), "jj git init should succeed");
        let repository = Repository::open(temporary.path()).expect("repository");
        let worker = repository
            .worker_pool()
            .resize_to(PoolCapacity::new(1).expect("capacity"))
            .expect("Worker Workspace should be provisioned")
            .added_workers()[0]
            .clone();
        (temporary, repository, worker)
    }

    /// A real background Run process the ordering test can own.
    fn real_background_run(log_path: PathBuf) -> BackgroundRun {
        let child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("background helper process");
        BackgroundRun {
            child,
            runtime: AgentRuntime::Claude,
            log_path,
            completion: None,
        }
    }

    /// A Reservation is released only after its process group is actually gone:
    /// when cleanup cannot confirm exit, the Worker must stay `busy` rather than
    /// become reusable while an untracked agent may still be running.
    #[test]
    fn a_reservation_survives_an_unconfirmed_untracked_cleanup() {
        let (temporary, repository, worker) = reserved_worker();
        let reservation = repository
            .worker_pool()
            .reserve_named(worker.clone(), "ENG-240")
            .expect("Worker reservation");
        let background = real_background_run(temporary.path().join("worker.log"));

        let error = persist_pid_failure_with(
            &reservation,
            background,
            RunSupervisorError::PersistPidMissing {
                worker: worker.clone(),
                pid: 1,
            },
            |background| {
                // Real process: end its group so the test does not leak, then
                // report the unconfirmed-cleanup outcome under test.
                let _ = cleanup_untracked_run(background);
                Err(io::Error::other("injected cleanup failure"))
            },
        )
        .expect_err("an unconfirmed cleanup must surface the failure");

        assert!(
            matches!(error, RunSupervisorError::PersistPidCleanup { .. }),
            "unexpected error: {error}"
        );
        let snapshot = repository.worker_pool().snapshot();
        let worker = snapshot.worker(worker.as_str()).expect("reserved Worker");
        assert_eq!(
            worker.status(),
            WorkerStatus::Busy,
            "a Worker whose untracked Run cleanup failed must not be released"
        );
    }
}
