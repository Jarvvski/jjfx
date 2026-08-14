//! The Workspace Dispatch command/event seam used by the jjfx App.
//!
//! The App submits a small command vocabulary and receives immutable events.
//! Adapters own repository access, locks, blocking work, and error mapping.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use wsg_core::{
    AgentRuntime, AgentRuntimeQuery, AgentSessionResolution, DirectDispatchError,
    DirectDispatchExecution, DirectDispatchFailurePhase, DirectDispatchOutcome,
    DirectDispatchRequest, DispatchGroup, DispatchGroupState, DispatchGroupStatusCounts,
    PI_DISCOVERY_HELPER_ENV, PiDiscoveryHelper, PoolCapacity, ReadyTicketFilter, RunActivity,
    RunMode, RunResult, SubIssueStatus, TicketDiscovery, TicketId, TicketStatus, WorkerId,
    WorkerPoolError, WorkerPoolSnapshot, WorkerStatus,
};

/// A user-visible operation identity used to ignore stale results.
pub type OperationId = u64;

/// Immutable latest activity from one Worker's provider-neutral Run log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerLogSnapshot {
    worker: String,
    runtime: AgentRuntime,
    activity: Option<RunActivity>,
    result: Option<RunResult>,
}

impl WorkerLogSnapshot {
    pub(crate) fn new(
        worker: impl Into<String>,
        runtime: AgentRuntime,
        activity: Option<RunActivity>,
        result: Option<RunResult>,
    ) -> Self {
        Self {
            worker: worker.into(),
            runtime,
            activity,
            result,
        }
    }

    /// Returns the Worker whose log was read.
    pub fn worker(&self) -> &str {
        &self.worker
    }

    /// Returns the provider selected for the Run log.
    pub const fn runtime(&self) -> AgentRuntime {
        self.runtime
    }

    /// Returns the latest normalized activity, if the log contains one.
    pub const fn activity(&self) -> Option<&RunActivity> {
        self.activity.as_ref()
    }

    /// Returns the terminal normalized result, if the Worker has one.
    pub const fn result(&self) -> Option<&RunResult> {
        self.result.as_ref()
    }
}

/// The Worker action that launched a Follow-up Run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerActionKind {
    /// Send a user-provided prompt to the Worker.
    Send,
    /// Ask the Worker to address Pull Request review feedback.
    Review,
}

impl WorkerActionKind {
    /// Returns the human-facing action name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Send => "Send",
            Self::Review => "Review",
        }
    }
}

/// Immutable presentation of a launched Worker Follow-up Run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerSessionOutcome {
    worker: String,
    action: WorkerActionKind,
    runtime: AgentRuntime,
    session: AgentSessionResolution,
    pid: u32,
}

impl WorkerSessionOutcome {
    pub(crate) fn new(
        worker: impl Into<String>,
        action: WorkerActionKind,
        runtime: AgentRuntime,
        session: AgentSessionResolution,
        pid: u32,
    ) -> Self {
        Self {
            worker: worker.into(),
            action,
            runtime,
            session,
            pid,
        }
    }

    /// Returns the Worker that owns the new Run.
    pub fn worker(&self) -> &str {
        &self.worker
    }

    /// Returns the action that launched the new Run.
    pub const fn action(&self) -> WorkerActionKind {
        self.action
    }

    /// Returns the Agent Runtime selected for the Run.
    pub const fn runtime(&self) -> AgentRuntime {
        self.runtime
    }

    /// Returns whether the prior Agent Session resumed or why a fresh one began.
    pub fn session(&self) -> &AgentSessionResolution {
        &self.session
    }

    /// Returns the Agent Runtime process identifier.
    pub const fn pid(&self) -> u32 {
        self.pid
    }
}

/// Immutable presentation of a Worker Run Reset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerResetOutcome {
    worker: String,
    run: wsg_core::RunReset,
}

impl WorkerResetOutcome {
    pub(crate) fn new(worker: impl Into<String>, run: wsg_core::RunReset) -> Self {
        Self {
            worker: worker.into(),
            run,
        }
    }

    /// Returns the Worker whose Run was Reset.
    pub fn worker(&self) -> &str {
        &self.worker
    }

    /// Returns how Run cleanup changed the Worker lifecycle.
    pub const fn run(&self) -> wsg_core::RunReset {
        self.run
    }
}

/// The result of asynchronous Worker Workspace restoration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceRestorationResult {
    /// No Workspace directory existed, so no restoration command was needed.
    Skipped,
    /// The Workspace was restored and returned to the trunk revision.
    Restored,
    /// Restoration failed after Reset released Worker capacity.
    Failed(String),
}

/// Reset result plus the independent Workspace restoration handle.
pub struct ResetAdapterResult {
    outcome: WorkerResetOutcome,
    restoration: wsg_core::WorkspaceRestoration,
}

impl ResetAdapterResult {
    fn new(outcome: WorkerResetOutcome, restoration: wsg_core::WorkspaceRestoration) -> Self {
        Self {
            outcome,
            restoration,
        }
    }

    fn into_parts(self) -> (WorkerResetOutcome, wsg_core::WorkspaceRestoration) {
        (self.outcome, self.restoration)
    }
}

/// How a Dismiss operation changed Worker capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerDismissDisposition {
    /// The idle Worker was removed from the Pool.
    Removed { capacity: usize },
    /// The terminal Worker was cleared in place.
    Reset,
}

/// A typed result from a non-Run Worker action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerCommandResult {
    /// A Worker bookmark was rebased and pushed.
    Rebased { worker: String, branch: String },
    /// A Worker's Pull Request was opened.
    PullRequestOpened { worker: String, branch: String },
    /// A Worker's cosmetic alias was set or cleared.
    AliasChanged {
        worker: String,
        alias: Option<String>,
    },
    /// A Worker was dismissed using the compatibility disposition.
    Dismissed {
        worker: String,
        disposition: WorkerDismissDisposition,
    },
}

impl WorkerCommandResult {
    /// Returns a concise human-facing result.
    pub fn notice(&self) -> String {
        match self {
            Self::Rebased { worker, branch } => format!("Rebased {worker} onto {branch}"),
            Self::PullRequestOpened { worker, branch } => {
                format!("Opened Pull Request for {worker} ({branch})")
            }
            Self::AliasChanged { worker, alias } => match alias {
                Some(alias) => format!("Named {worker} -> {alias}"),
                None => format!("Cleared alias for {worker}"),
            },
            Self::Dismissed {
                worker,
                disposition: WorkerDismissDisposition::Removed { capacity },
            } => format!("Dismissed {worker} (Pool capacity: {capacity})"),
            Self::Dismissed {
                worker,
                disposition: WorkerDismissDisposition::Reset,
            } => format!("Reset {worker} to idle without restoring its Workspace"),
        }
    }
}

/// Commands accepted by the Workspace Dispatch controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceDispatchCommand {
    /// Reconcile recorded Runs and read a complete immutable Pool snapshot.
    Refresh { operation: OperationId },
    /// Set the exact Pool capacity, creating the Pool when necessary.
    Resize {
        operation: OperationId,
        capacity: usize,
    },
    /// Destroy the Pool after the caller has obtained confirmation.
    Destroy { operation: OperationId },
    /// Dispatch one or more Ticket IDs, optionally targeting one exact Worker.
    Dispatch {
        operation: OperationId,
        tickets: Vec<String>,
        worker: Option<String>,
    },
    /// Retry a Dispatch after approving the exact capacity gap.
    DispatchWithApprovedGrowth {
        operation: OperationId,
        tickets: Vec<String>,
        worker: Option<String>,
        additional: usize,
    },
    /// Dispatch only the currently available subset after declining growth.
    DispatchUseAvailable {
        operation: OperationId,
        tickets: Vec<String>,
        worker: Option<String>,
    },
    /// Discover provider-ready Tickets for the configured dispatch label.
    DiscoverReady {
        operation: OperationId,
        label: String,
    },
    /// Start or resume one Parent Ticket's dependency-aware orchestration.
    Orchestrate {
        operation: OperationId,
        parent: String,
    },
    /// Send a prompt to one selected Worker.
    Send {
        operation: OperationId,
        worker: String,
        prompt: String,
    },
    /// Start a Pull Request review on one selected Worker.
    Review {
        operation: OperationId,
        worker: String,
    },
    /// Reset one selected Worker after the caller has obtained confirmation.
    Reset {
        operation: OperationId,
        worker: String,
    },
    /// Rebase and push one Worker's bookmark.
    Rebase {
        operation: OperationId,
        worker: String,
    },
    /// Open one Worker's Pull Request.
    OpenPullRequest {
        operation: OperationId,
        worker: String,
    },
    /// Set or clear one Worker's cosmetic alias.
    SetAlias {
        operation: OperationId,
        worker: String,
        alias: String,
    },
    /// Dismiss one Worker after the caller has obtained confirmation.
    Dismiss {
        operation: OperationId,
        worker: String,
    },
    /// Start one cancellable latest-activity Worker log watcher.
    WatchWorkerLog {
        operation: OperationId,
        worker: String,
    },
    /// Stop the Worker log watcher identified by its operation.
    StopWorkerLog { operation: OperationId },
}

/// The result of a Pool membership mutation, reduced to presentation data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolMutationResult {
    capacity: usize,
    added_workers: Vec<String>,
    removed_workers: Vec<String>,
}

/// A presentation-safe outcome for one Direct Dispatch Ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchOutcome {
    ticket: String,
    title: String,
    worker: Option<String>,
    pid: Option<u32>,
    phase: Option<String>,
    detail: Option<String>,
}

impl DispatchOutcome {
    /// Creates a successful background-launch outcome.
    pub fn success(ticket: String, title: String, worker: String, pid: u32) -> Self {
        Self {
            ticket,
            title,
            worker: Some(worker),
            pid: Some(pid),
            phase: None,
            detail: None,
        }
    }

    /// Creates a failed pre-launch outcome.
    pub fn failure(
        ticket: String,
        title: String,
        worker: Option<String>,
        phase: DirectDispatchFailurePhase,
        detail: String,
    ) -> Self {
        Self {
            ticket,
            title,
            worker,
            pid: None,
            phase: Some(format!("{phase:?}")),
            detail: Some(detail),
        }
    }

    /// Returns the stable Ticket identifier.
    pub fn ticket(&self) -> &str {
        &self.ticket
    }
    /// Returns the human-facing Ticket title.
    pub fn title(&self) -> &str {
        &self.title
    }
    /// Returns the selected Worker, when one was assigned.
    pub fn worker(&self) -> Option<&str> {
        self.worker.as_deref()
    }
    /// Returns the Agent Runtime process identifier, when launched in background.
    pub const fn pid(&self) -> Option<u32> {
        self.pid
    }
    /// Returns the failure phase, when Dispatch failed.
    pub fn phase(&self) -> Option<&str> {
        self.phase.as_deref()
    }
    /// Returns failure detail, when Dispatch failed.
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
    /// Reports whether the Ticket launched successfully.
    pub const fn succeeded(&self) -> bool {
        self.pid.is_some()
    }
}

/// Ordered presentation outcomes from one Direct Dispatch batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchResult {
    outcomes: Vec<DispatchOutcome>,
    partial: bool,
}

impl DispatchResult {
    /// Creates ordered presentation outcomes from one Dispatch attempt.
    #[cfg(test)]
    pub fn new(outcomes: Vec<DispatchOutcome>, partial: bool) -> Self {
        Self { outcomes, partial }
    }

    /// Returns outcomes in the same order as the requested Tickets.
    pub fn outcomes(&self) -> &[DispatchOutcome] {
        &self.outcomes
    }
    /// Reports whether the shared coordinator dispatched only a subset.
    pub const fn is_partial(&self) -> bool {
        self.partial
    }
}

/// Structured Dispatch failure information retained for capacity confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DispatchCapacityShortage {
    requested: usize,
    available: usize,
}

impl DispatchCapacityShortage {
    /// Creates a shortage from counts observed under the Pool lock.
    #[cfg(test)]
    pub const fn new(requested: usize, available: usize) -> Self {
        Self {
            requested,
            available,
        }
    }

    /// Returns the number of requested Tickets.
    pub const fn requested(self) -> usize {
        self.requested
    }
    /// Returns the idle Worker count observed under the Pool lock.
    pub const fn available(self) -> usize {
        self.available
    }
    /// Returns the exact additional capacity needed at observation time.
    pub const fn gap(self) -> usize {
        self.requested.saturating_sub(self.available)
    }
}

/// Errors emitted by Dispatch while preserving typed capacity shortages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceDispatchError {
    /// The Pool did not have enough idle Workers for the complete batch.
    CapacityShortage(DispatchCapacityShortage),
    /// The requested Dispatch could not be prepared or launched.
    Failed(String),
}

/// A discovered Ticket reduced to immutable preview data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyTicket {
    id: String,
    title: String,
}

impl ReadyTicket {
    /// Creates immutable preview data for one discovered Ticket.
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
        }
    }

    /// Returns the stable Ticket identifier.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Returns the human-facing Ticket title.
    pub fn title(&self) -> &str {
        &self.title
    }
}

/// Immutable Dispatch Group progress reduced to presentation data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchGroupProgress {
    parent: String,
    issues: Vec<DispatchIssueProgress>,
    ready: Vec<String>,
    maximum_wave: usize,
    counts: DispatchGroupStatusCounts,
    terminal: bool,
}

impl DispatchGroupProgress {
    // The orchestration adapter consumes this projection in the next vertical slice.
    #[allow(dead_code)]
    pub(crate) fn from_state(state: DispatchGroupState) -> Result<Self, String> {
        let group = DispatchGroup::from_state(state).map_err(|error| error.to_string())?;
        let state = group.state();
        let mut waves = BTreeMap::new();
        let issues = state
            .sub_issues
            .iter()
            .map(|(ticket, issue)| {
                let status =
                    SubIssueStatus::try_from(&issue.status).map_err(|error| error.to_string())?;
                let wave = dependency_wave(ticket, state, &mut waves);
                Ok(DispatchIssueProgress {
                    ticket: ticket.to_string(),
                    title: issue.title.clone(),
                    status,
                    blockers: issue.blocked_by.iter().map(ToString::to_string).collect(),
                    worker: issue.worker.as_ref().map(ToString::to_string),
                    retries: issue.retries,
                    wave,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let ready = group.ready().iter().map(ToString::to_string).collect();
        Ok(Self {
            parent: state.parent.to_string(),
            issues,
            ready,
            maximum_wave: group.maximum_wave_size(),
            counts: group.status_counts(),
            terminal: group.is_terminal(),
        })
    }

    /// Returns the Parent Ticket identifier.
    pub fn parent(&self) -> &str {
        &self.parent
    }

    /// Returns the stable, provider-order-independent Sub-issue rows.
    pub fn issues(&self) -> &[DispatchIssueProgress] {
        &self.issues
    }

    /// Returns currently dispatchable Tickets in stable order.
    pub fn ready(&self) -> &[String] {
        &self.ready
    }

    /// Returns the largest dependency wave width.
    pub const fn maximum_wave(&self) -> usize {
        self.maximum_wave
    }

    /// Returns terminal outcome counts.
    pub const fn counts(&self) -> DispatchGroupStatusCounts {
        self.counts
    }

    /// Reports whether all Sub-issues have reached terminal states.
    pub const fn is_terminal(&self) -> bool {
        self.terminal
    }
}

/// One immutable Sub-issue row in Dispatch Group progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchIssueProgress {
    ticket: String,
    title: String,
    status: SubIssueStatus,
    blockers: Vec<String>,
    worker: Option<String>,
    retries: i64,
    wave: usize,
}

impl DispatchIssueProgress {
    /// Returns the Sub-issue identifier.
    pub fn ticket(&self) -> &str {
        &self.ticket
    }

    /// Returns the human-facing Sub-issue title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the normalized lifecycle status.
    pub const fn status(&self) -> SubIssueStatus {
        self.status
    }

    /// Returns direct dependency identifiers.
    pub fn blockers(&self) -> &[String] {
        &self.blockers
    }

    /// Returns the assigned Worker, when the Sub-issue is dispatched.
    pub fn worker(&self) -> Option<&str> {
        self.worker.as_deref()
    }

    /// Returns the number of completed retries.
    pub const fn retries(&self) -> i64 {
        self.retries
    }

    /// Returns the derived dependency wave number.
    pub const fn wave(&self) -> usize {
        self.wave
    }
}

// The orchestration adapter consumes this projection in the next vertical slice.
#[allow(dead_code)]
fn dependency_wave(
    ticket: &TicketId,
    state: &DispatchGroupState,
    waves: &mut BTreeMap<TicketId, usize>,
) -> usize {
    if let Some(wave) = waves.get(ticket) {
        return *wave;
    }
    let Some(issue) = state.sub_issues.get(ticket) else {
        return 0;
    };
    let status = match SubIssueStatus::try_from(&issue.status) {
        Ok(status) => status,
        Err(_) => return 0,
    };
    let wave = if status == SubIssueStatus::Skipped {
        0
    } else {
        issue
            .blocked_by
            .iter()
            .map(|blocker| dependency_wave(blocker, state, waves))
            .max()
            .unwrap_or(0)
            .saturating_add(1)
    };
    waves.insert(ticket.clone(), wave);
    wave
}

fn orchestration_notice(event: &wsg_core::OrchestrationEvent) -> Option<String> {
    match event {
        wsg_core::OrchestrationEvent::Started { .. }
        | wsg_core::OrchestrationEvent::Terminal(_) => None,
        wsg_core::OrchestrationEvent::Completed { ticket, worker, .. } => {
            Some(format!("{ticket} completed on {worker}"))
        }
        wsg_core::OrchestrationEvent::Retrying {
            ticket, attempt, ..
        } => Some(format!("retrying {ticket} (attempt {attempt})")),
        wsg_core::OrchestrationEvent::Dispatched { ticket, worker } => {
            Some(format!("{ticket} dispatched to {worker}"))
        }
        wsg_core::OrchestrationEvent::WaitingForCapacity { ticket } => {
            Some(format!("waiting for Worker capacity: {ticket}"))
        }
        wsg_core::OrchestrationEvent::LaunchFailed { ticket, detail, .. } => {
            Some(format!("launch failed {ticket}: {detail}"))
        }
        wsg_core::OrchestrationEvent::BranchRevalidated {
            ticket, current, ..
        } => Some(format!("repaired {ticket} -> {current}")),
        wsg_core::OrchestrationEvent::Failed { ticket, detail, .. } => Some(format!(
            "{ticket} failed: {}",
            detail.as_deref().unwrap_or("unknown error")
        )),
    }
}

/// Ready Ticket discovery results, including safely excluded entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyTicketResult {
    tickets: Vec<ReadyTicket>,
    diagnostics: Vec<String>,
}

impl ReadyTicketResult {
    /// Creates an ordered discovery result and its validation diagnostics.
    pub fn new(tickets: Vec<ReadyTicket>, diagnostics: Vec<String>) -> Self {
        Self {
            tickets,
            diagnostics,
        }
    }

    /// Returns Tickets in provider order.
    pub fn tickets(&self) -> &[ReadyTicket] {
        &self.tickets
    }
    /// Returns diagnostics for entries excluded by validation.
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }
}

/// Result of a successful adapter Dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchAdapterResult {
    result: DispatchResult,
}

impl DispatchAdapterResult {
    fn new(result: DispatchResult) -> Self {
        Self { result }
    }
    fn into_result(self) -> DispatchResult {
        self.result
    }
}

impl PoolMutationResult {
    /// Returns the resulting Pool capacity.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns Workers created by the mutation.
    pub fn added_workers(&self) -> &[String] {
        &self.added_workers
    }

    /// Returns Workers removed by the mutation.
    pub fn removed_workers(&self) -> &[String] {
        &self.removed_workers
    }
}

/// Immutable updates emitted by an orchestration adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceDispatchOrchestrationEvent {
    /// Preparation completed and the group is ready to run.
    Started { parent: String, resumed: bool },
    /// A durable group transition was folded into presentation data.
    Progress {
        progress: DispatchGroupProgress,
        notice: Option<String>,
    },
    /// A Parent without Sub-issues launched through Direct Dispatch.
    Direct {
        parent: String,
        worker: String,
        pid: u32,
    },
    /// The group reached a terminal state.
    Terminal {
        parent: String,
        counts: DispatchGroupStatusCounts,
    },
}

/// Events emitted after a command completes outside the App event loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceDispatchEvent {
    /// The newest immutable Pool presentation snapshot.
    Snapshot {
        operation: OperationId,
        snapshot: WorkerPoolSnapshot,
    },
    /// A Pool mutation completed and its state is being refreshed.
    Resized {
        operation: OperationId,
        result: PoolMutationResult,
    },
    /// Pool destruction completed.
    Destroyed { operation: OperationId },
    /// Dispatch completed with ordered per-Ticket outcomes.
    Dispatched {
        operation: OperationId,
        result: DispatchResult,
    },
    /// Dispatch found too little idle capacity for the complete batch.
    DispatchCapacity {
        operation: OperationId,
        tickets: Vec<String>,
        worker: Option<String>,
        shortage: DispatchCapacityShortage,
    },
    /// Ready Ticket discovery completed with ordered preview data.
    ReadyTickets {
        operation: OperationId,
        result: ReadyTicketResult,
    },
    /// The latest committed Dispatch Group progress projection.
    // The orchestration stream constructs this event in the next vertical slice.
    #[allow(dead_code)]
    GroupProgress {
        operation: OperationId,
        progress: DispatchGroupProgress,
    },
    /// Orchestration preparation or execution started.
    OrchestrationStarted {
        operation: OperationId,
        parent: String,
        resumed: bool,
    },
    /// A durable orchestration update was projected for the Pool view.
    OrchestrationProgress {
        operation: OperationId,
        progress: DispatchGroupProgress,
        notice: Option<String>,
    },
    /// A Parent without children launched directly.
    OrchestrationDirect {
        operation: OperationId,
        parent: String,
        worker: String,
        pid: u32,
    },
    /// Orchestration reached a terminal group state.
    OrchestrationTerminal {
        operation: OperationId,
        parent: String,
        counts: DispatchGroupStatusCounts,
    },
    /// A Worker Follow-up Run was launched and its Session outcome resolved.
    WorkerActionCompleted {
        operation: OperationId,
        outcome: WorkerSessionOutcome,
    },
    /// A Worker Run was Reset; Workspace restoration may still be pending.
    WorkerResetCompleted {
        operation: OperationId,
        outcome: WorkerResetOutcome,
    },
    /// A Worker Workspace restoration finished separately from Reset.
    WorkspaceRestorationCompleted {
        operation: OperationId,
        worker: String,
        result: WorkspaceRestorationResult,
    },
    /// A non-Run Worker action completed with a typed result.
    WorkerCommandCompleted {
        operation: OperationId,
        result: WorkerCommandResult,
    },
    /// A changed provider-neutral Worker log snapshot.
    WorkerLogUpdated {
        operation: OperationId,
        snapshot: Box<WorkerLogSnapshot>,
    },
    /// A Worker log could not be read.
    WorkerLogFailed {
        operation: OperationId,
        worker: String,
        message: String,
    },
    /// A command or its post-mutation refresh failed.
    Failed {
        operation: OperationId,
        message: String,
    },
}

/// The blocking implementation behind the controller seam.
pub trait WorkspaceDispatchAdapter: Send + Sync + 'static {
    /// Reconcile and read the current immutable Pool presentation.
    fn refresh(&self) -> Result<WorkerPoolSnapshot, String>;
    /// Set exact Pool capacity.
    fn resize(&self, capacity: usize) -> Result<PoolMutationResult, String>;
    /// Destroy every Pool Worker and its compatible state.
    fn destroy(&self) -> Result<(), String>;
    /// Dispatch Ticket IDs through the shared Direct Dispatch coordinator.
    fn dispatch(
        &self,
        tickets: &[String],
        worker: Option<&str>,
    ) -> Result<DispatchAdapterResult, WorkspaceDispatchError>;
    /// Dispatch after approving the exact capacity gap under the Pool lock.
    fn dispatch_with_approved_growth(
        &self,
        tickets: &[String],
        worker: Option<&str>,
        additional: usize,
    ) -> Result<DispatchAdapterResult, WorkspaceDispatchError>;
    /// Dispatch only the available subset under the Pool lock.
    fn dispatch_use_available(
        &self,
        tickets: &[String],
        worker: Option<&str>,
    ) -> Result<DispatchAdapterResult, WorkspaceDispatchError>;
    /// Discover Tickets ready for Dispatch through the configured Agent Runtime.
    fn discover_ready(&self, label: &str) -> Result<ReadyTicketResult, String>;
    /// Prepare and run one Parent orchestration, emitting durable progress updates.
    fn orchestrate(
        &self,
        parent: &str,
        emit: &mut dyn FnMut(WorkspaceDispatchOrchestrationEvent),
    ) -> Result<(), String>;
    /// Launch a user prompt as a background Follow-up Run.
    fn send(&self, worker: &str, prompt: &str) -> Result<WorkerSessionOutcome, String>;
    /// Launch a Pull Request review as a background Follow-up Run.
    fn review(&self, worker: &str) -> Result<WorkerSessionOutcome, String>;
    /// Reset a Worker and return its independent Workspace restoration handle.
    fn reset(&self, worker: &str) -> Result<ResetAdapterResult, String>;
    /// Rebase and push one Worker's bookmark.
    fn rebase(&self, worker: &str) -> Result<WorkerCommandResult, String>;
    /// Open one Worker's Pull Request.
    fn open_pull_request(&self, worker: &str) -> Result<WorkerCommandResult, String>;
    /// Set or clear one Worker's cosmetic alias.
    fn set_alias(&self, worker: &str, alias: &str) -> Result<WorkerCommandResult, String>;
    /// Dismiss one Worker using the compatibility disposition.
    fn dismiss(&self, worker: &str) -> Result<WorkerCommandResult, String>;
    /// Reads one latest Worker log snapshot without choosing a rendering.
    fn worker_log(&self, worker: &str) -> Result<WorkerLogSnapshot, String>;
}

struct WatcherRegistry {
    active: Mutex<BTreeMap<OperationId, Arc<AtomicBool>>>,
}

impl WatcherRegistry {
    fn new() -> Self {
        Self {
            active: Mutex::new(BTreeMap::new()),
        }
    }

    fn stop(&self, operation: OperationId) {
        if let Some(cancel) = self
            .active
            .lock()
            .expect("Worker log watcher registry lock")
            .remove(&operation)
        {
            cancel.store(true, Ordering::Release);
        }
    }
}

impl Drop for WatcherRegistry {
    fn drop(&mut self) {
        for cancel in self
            .active
            .get_mut()
            .expect("Worker log watcher registry lock")
            .values()
        {
            cancel.store(true, Ordering::Release);
        }
    }
}

/// A deep asynchronous controller for Workspace Dispatch operations.
#[derive(Clone)]
pub struct WorkspaceDispatchController {
    adapter: Arc<dyn WorkspaceDispatchAdapter>,
    emit: Arc<dyn Fn(WorkspaceDispatchEvent) + Send + Sync>,
    watchers: Arc<WatcherRegistry>,
}

impl WorkspaceDispatchController {
    /// Creates a controller from an adapter and an event sink.
    pub fn new<A, E>(adapter: A, emit: E) -> Self
    where
        A: WorkspaceDispatchAdapter,
        E: Fn(WorkspaceDispatchEvent) + Send + Sync + 'static,
    {
        Self {
            adapter: Arc::new(adapter),
            emit: Arc::new(emit),
            watchers: Arc::new(WatcherRegistry::new()),
        }
    }

    /// Submits a blocking command and returns immediately.
    pub fn submit(&self, command: WorkspaceDispatchCommand) {
        let adapter = Arc::clone(&self.adapter);
        let emit = Arc::clone(&self.emit);
        let watchers = Arc::clone(&self.watchers);
        std::thread::spawn(move || match command {
            WorkspaceDispatchCommand::Refresh { operation } => match adapter.refresh() {
                Ok(snapshot) => emit(WorkspaceDispatchEvent::Snapshot {
                    operation,
                    snapshot,
                }),
                Err(message) => emit(WorkspaceDispatchEvent::Failed { operation, message }),
            },
            WorkspaceDispatchCommand::Resize {
                operation,
                capacity,
            } => match adapter.resize(capacity) {
                Ok(result) => {
                    emit(WorkspaceDispatchEvent::Resized { operation, result });
                    match adapter.refresh() {
                        Ok(snapshot) => emit(WorkspaceDispatchEvent::Snapshot {
                            operation,
                            snapshot,
                        }),
                        Err(message) => {
                            emit(WorkspaceDispatchEvent::Failed { operation, message });
                        }
                    }
                }
                Err(message) => emit(WorkspaceDispatchEvent::Failed { operation, message }),
            },
            WorkspaceDispatchCommand::Destroy { operation } => match adapter.destroy() {
                Ok(()) => {
                    emit(WorkspaceDispatchEvent::Destroyed { operation });
                    match adapter.refresh() {
                        Ok(snapshot) => emit(WorkspaceDispatchEvent::Snapshot {
                            operation,
                            snapshot,
                        }),
                        Err(message) => {
                            emit(WorkspaceDispatchEvent::Failed { operation, message });
                        }
                    }
                }
                Err(message) => emit(WorkspaceDispatchEvent::Failed { operation, message }),
            },
            WorkspaceDispatchCommand::Dispatch {
                operation,
                tickets,
                worker,
            } => match adapter.dispatch(&tickets, worker.as_deref()) {
                Ok(result) => emit(WorkspaceDispatchEvent::Dispatched {
                    operation,
                    result: result.into_result(),
                }),
                Err(WorkspaceDispatchError::CapacityShortage(shortage)) => {
                    emit(WorkspaceDispatchEvent::DispatchCapacity {
                        operation,
                        tickets,
                        worker,
                        shortage,
                    })
                }
                Err(WorkspaceDispatchError::Failed(message)) => {
                    emit(WorkspaceDispatchEvent::Failed { operation, message })
                }
            },
            WorkspaceDispatchCommand::DispatchWithApprovedGrowth {
                operation,
                tickets,
                worker,
                additional,
            } => {
                match adapter.dispatch_with_approved_growth(&tickets, worker.as_deref(), additional)
                {
                    Ok(result) => emit(WorkspaceDispatchEvent::Dispatched {
                        operation,
                        result: result.into_result(),
                    }),
                    Err(WorkspaceDispatchError::CapacityShortage(shortage)) => {
                        emit(WorkspaceDispatchEvent::DispatchCapacity {
                            operation,
                            tickets,
                            worker,
                            shortage,
                        })
                    }
                    Err(WorkspaceDispatchError::Failed(message)) => {
                        emit(WorkspaceDispatchEvent::Failed { operation, message })
                    }
                }
            }
            WorkspaceDispatchCommand::DispatchUseAvailable {
                operation,
                tickets,
                worker,
            } => match adapter.dispatch_use_available(&tickets, worker.as_deref()) {
                Ok(result) => emit(WorkspaceDispatchEvent::Dispatched {
                    operation,
                    result: result.into_result(),
                }),
                Err(WorkspaceDispatchError::CapacityShortage(shortage)) => {
                    emit(WorkspaceDispatchEvent::DispatchCapacity {
                        operation,
                        tickets,
                        worker,
                        shortage,
                    })
                }
                Err(WorkspaceDispatchError::Failed(message)) => {
                    emit(WorkspaceDispatchEvent::Failed { operation, message })
                }
            },
            WorkspaceDispatchCommand::DiscoverReady { operation, label } => {
                match adapter.discover_ready(&label) {
                    Ok(result) => emit(WorkspaceDispatchEvent::ReadyTickets { operation, result }),
                    Err(message) => emit(WorkspaceDispatchEvent::Failed { operation, message }),
                }
            }
            WorkspaceDispatchCommand::Orchestrate { operation, parent } => {
                let mut forward = |event| match event {
                    WorkspaceDispatchOrchestrationEvent::Started { parent, resumed } => {
                        emit(WorkspaceDispatchEvent::OrchestrationStarted {
                            operation,
                            parent,
                            resumed,
                        });
                    }
                    WorkspaceDispatchOrchestrationEvent::Progress { progress, notice } => {
                        emit(WorkspaceDispatchEvent::OrchestrationProgress {
                            operation,
                            progress,
                            notice,
                        });
                    }
                    WorkspaceDispatchOrchestrationEvent::Direct {
                        parent,
                        worker,
                        pid,
                    } => {
                        emit(WorkspaceDispatchEvent::OrchestrationDirect {
                            operation,
                            parent,
                            worker,
                            pid,
                        });
                    }
                    WorkspaceDispatchOrchestrationEvent::Terminal { parent, counts } => {
                        emit(WorkspaceDispatchEvent::OrchestrationTerminal {
                            operation,
                            parent,
                            counts,
                        });
                    }
                };
                if let Err(message) = adapter.orchestrate(&parent, &mut forward) {
                    emit(WorkspaceDispatchEvent::Failed { operation, message });
                }
            }
            WorkspaceDispatchCommand::Send {
                operation,
                worker,
                prompt,
            } => match adapter.send(&worker, &prompt) {
                Ok(outcome) => {
                    emit(WorkspaceDispatchEvent::WorkerActionCompleted { operation, outcome })
                }
                Err(message) => emit(WorkspaceDispatchEvent::Failed { operation, message }),
            },
            WorkspaceDispatchCommand::Review { operation, worker } => {
                match adapter.review(&worker) {
                    Ok(outcome) => {
                        emit(WorkspaceDispatchEvent::WorkerActionCompleted { operation, outcome })
                    }
                    Err(message) => emit(WorkspaceDispatchEvent::Failed { operation, message }),
                }
            }
            WorkspaceDispatchCommand::Reset { operation, worker } => match adapter.reset(&worker) {
                Ok(result) => {
                    let (outcome, restoration) = result.into_parts();
                    emit(WorkspaceDispatchEvent::WorkerResetCompleted { operation, outcome });
                    match adapter.refresh() {
                        Ok(snapshot) => emit(WorkspaceDispatchEvent::Snapshot {
                            operation,
                            snapshot,
                        }),
                        Err(message) => emit(WorkspaceDispatchEvent::Failed { operation, message }),
                    }
                    let restoration_emit = Arc::clone(&emit);
                    std::thread::spawn(move || {
                        let result = match restoration {
                            wsg_core::WorkspaceRestoration::SkippedMissingWorkspace => {
                                WorkspaceRestorationResult::Skipped
                            }
                            wsg_core::WorkspaceRestoration::Pending(handle) => {
                                match handle.wait() {
                                    Ok(()) => WorkspaceRestorationResult::Restored,
                                    Err(error) => {
                                        WorkspaceRestorationResult::Failed(error.to_string())
                                    }
                                }
                            }
                        };
                        restoration_emit(WorkspaceDispatchEvent::WorkspaceRestorationCompleted {
                            operation,
                            worker,
                            result,
                        });
                    });
                }
                Err(message) => emit(WorkspaceDispatchEvent::Failed { operation, message }),
            },
            WorkspaceDispatchCommand::Rebase { operation, worker } => {
                match adapter.rebase(&worker) {
                    Ok(result) => {
                        emit(WorkspaceDispatchEvent::WorkerCommandCompleted { operation, result })
                    }
                    Err(message) => emit(WorkspaceDispatchEvent::Failed { operation, message }),
                }
            }
            WorkspaceDispatchCommand::OpenPullRequest { operation, worker } => {
                match adapter.open_pull_request(&worker) {
                    Ok(result) => {
                        emit(WorkspaceDispatchEvent::WorkerCommandCompleted { operation, result })
                    }
                    Err(message) => emit(WorkspaceDispatchEvent::Failed { operation, message }),
                }
            }
            WorkspaceDispatchCommand::SetAlias {
                operation,
                worker,
                alias,
            } => match adapter.set_alias(&worker, &alias) {
                Ok(result) => {
                    emit(WorkspaceDispatchEvent::WorkerCommandCompleted { operation, result })
                }
                Err(message) => emit(WorkspaceDispatchEvent::Failed { operation, message }),
            },
            WorkspaceDispatchCommand::Dismiss { operation, worker } => {
                match adapter.dismiss(&worker) {
                    Ok(result) => {
                        emit(WorkspaceDispatchEvent::WorkerCommandCompleted { operation, result })
                    }
                    Err(message) => emit(WorkspaceDispatchEvent::Failed { operation, message }),
                }
            }
            WorkspaceDispatchCommand::WatchWorkerLog { operation, worker } => {
                let cancel = Arc::new(AtomicBool::new(false));
                if let Some(previous) = watchers
                    .active
                    .lock()
                    .expect("watcher registry lock")
                    .insert(operation, Arc::clone(&cancel))
                {
                    previous.store(true, Ordering::Release);
                }
                let watchers = Arc::clone(&watchers);
                std::thread::spawn(move || {
                    let mut previous = None;
                    loop {
                        if cancel.load(Ordering::Acquire) {
                            break;
                        }
                        match adapter.worker_log(&worker) {
                            Ok(snapshot) => {
                                let terminal = snapshot.result().is_some();
                                if previous.as_ref() != Some(&snapshot) {
                                    emit(WorkspaceDispatchEvent::WorkerLogUpdated {
                                        operation,
                                        snapshot: Box::new(snapshot.clone()),
                                    });
                                    previous = Some(snapshot);
                                }
                                if terminal {
                                    break;
                                }
                            }
                            Err(message) => {
                                emit(WorkspaceDispatchEvent::WorkerLogFailed {
                                    operation,
                                    worker: worker.clone(),
                                    message,
                                });
                                break;
                            }
                        }
                        for _ in 0..20 {
                            if cancel.load(Ordering::Acquire) {
                                break;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                    }
                    let mut active = watchers.active.lock().expect("watcher registry lock");
                    if active
                        .get(&operation)
                        .is_some_and(|current| Arc::ptr_eq(current, &cancel))
                    {
                        active.remove(&operation);
                    }
                });
            }
            WorkspaceDispatchCommand::StopWorkerLog { operation } => {
                watchers.stop(operation);
            }
        });
    }
}

/// The production adapter over the shared Repository-owned Pool module.
#[derive(Debug, Clone)]
pub struct RealWorkspaceDispatch {
    repository_root: PathBuf,
}

fn configured_ticket_query(
    repository: &wsg_core::Repository,
    runtime: AgentRuntime,
) -> AgentRuntimeQuery {
    let query = AgentRuntimeQuery::new(runtime, repository.root());
    if runtime != AgentRuntime::Pi {
        return query;
    }
    match std::env::var_os(PI_DISCOVERY_HELPER_ENV).filter(|executable| !executable.is_empty()) {
        Some(executable) => query.with_pi_helper(PiDiscoveryHelper::new(executable)),
        None => query,
    }
}

enum DispatchStrategy {
    Complete,
    ApprovedGrowth(usize),
    Available,
}

impl RealWorkspaceDispatch {
    /// Creates an adapter rooted at a discovered jj workspace.
    pub fn new(repository_root: impl Into<PathBuf>) -> Self {
        Self {
            repository_root: repository_root.into(),
        }
    }

    fn repository(&self) -> Result<wsg_core::Repository, String> {
        wsg_core::Repository::open(&self.repository_root).map_err(|error| error.to_string())
    }

    fn dispatch_with_strategy(
        &self,
        tickets: &[String],
        worker: Option<&str>,
        strategy: DispatchStrategy,
    ) -> Result<DispatchAdapterResult, WorkspaceDispatchError> {
        let repository = self.repository().map_err(WorkspaceDispatchError::Failed)?;
        let requests = tickets
            .iter()
            .map(|ticket| {
                let id = TicketId::parse(ticket.clone())
                    .map_err(|error| WorkspaceDispatchError::Failed(error.to_string()))?;
                let request = DirectDispatchRequest::for_ticket_id(id, RunMode::Background)
                    .map_err(|error| WorkspaceDispatchError::Failed(error.to_string()))?;
                Ok(match worker {
                    Some(worker) => request.to_worker(
                        WorkerId::parse(worker.to_owned())
                            .map_err(|error| WorkspaceDispatchError::Failed(error.to_string()))?,
                    ),
                    None => request,
                })
            })
            .collect::<Result<Vec<_>, WorkspaceDispatchError>>()?;
        let dispatcher = repository.direct_dispatch();
        let result = match strategy {
            DispatchStrategy::Complete => dispatcher.dispatch(&requests),
            DispatchStrategy::ApprovedGrowth(additional) => {
                dispatcher.dispatch_with_approved_growth(&requests, additional)
            }
            DispatchStrategy::Available => dispatcher.dispatch_use_available(&requests),
        }
        .map_err(|error| match error {
            DirectDispatchError::WorkerPool(WorkerPoolError::CapacityShortage(shortage)) => {
                WorkspaceDispatchError::CapacityShortage(DispatchCapacityShortage {
                    requested: shortage.requested(),
                    available: shortage.available(),
                })
            }
            other => WorkspaceDispatchError::Failed(other.to_string()),
        })?;
        let outcomes = result
            .outcomes()
            .iter()
            .map(|outcome| match outcome {
                DirectDispatchOutcome::Succeeded(success) => match success.execution() {
                    DirectDispatchExecution::Background { pid } => DispatchOutcome::success(
                        success.ticket().id().to_string(),
                        success.ticket().title().as_str().to_owned(),
                        success.worker().to_string(),
                        *pid,
                    ),
                    DirectDispatchExecution::Foreground(_) => DispatchOutcome::failure(
                        success.ticket().id().to_string(),
                        success.ticket().title().as_str().to_owned(),
                        Some(success.worker().to_string()),
                        DirectDispatchFailurePhase::Launch,
                        "foreground Dispatch is not supported by the jjfx controller".to_owned(),
                    ),
                },
                DirectDispatchOutcome::Failed(failure) => DispatchOutcome::failure(
                    failure.ticket().id().to_string(),
                    failure.ticket().title().as_str().to_owned(),
                    failure.worker().map(ToString::to_string),
                    failure.phase(),
                    failure.detail().to_owned(),
                ),
            })
            .collect();
        Ok(DispatchAdapterResult::new(DispatchResult {
            outcomes,
            partial: result.is_partial(),
        }))
    }

    fn worker_action(
        &self,
        worker: &str,
        action: WorkerActionKind,
        prompt: Option<&str>,
    ) -> Result<WorkerSessionOutcome, String> {
        let repository = self.repository()?;
        let worker_id = WorkerId::parse(worker.to_owned()).map_err(|error| error.to_string())?;
        let actions = wsg_core::WorkerActions::new(repository);
        let outcome = match action {
            WorkerActionKind::Send => actions
                .send(
                    &worker_id,
                    prompt.ok_or_else(|| "Send prompt cannot be missing".to_owned())?,
                    RunMode::Background,
                )
                .map_err(|error| error.to_string())?,
            WorkerActionKind::Review => actions
                .review(&worker_id, RunMode::Background)
                .map_err(|error| error.to_string())?,
        };
        let runtime = outcome.runtime();
        let session = outcome.session().clone();
        let wsg_core::FollowUpExecution::Background(run) = outcome.into_execution() else {
            return Err("foreground Worker actions are not supported by jjfx".to_owned());
        };
        let pid = run.pid();
        std::thread::spawn(move || {
            let _ = run.wait();
        });
        Ok(WorkerSessionOutcome::new(
            worker.to_owned(),
            action,
            runtime,
            session,
            pid,
        ))
    }
}

impl WorkspaceDispatchAdapter for RealWorkspaceDispatch {
    fn refresh(&self) -> Result<WorkerPoolSnapshot, String> {
        Ok(self.repository()?.worker_pool().reconcile_runs())
    }

    fn resize(&self, capacity: usize) -> Result<PoolMutationResult, String> {
        let capacity = PoolCapacity::new(capacity).map_err(|error| error.to_string())?;
        let result = self
            .repository()?
            .worker_pool()
            .resize_to(capacity)
            .map_err(|error| error.to_string())?;
        Ok(PoolMutationResult {
            capacity: result.capacity().as_usize(),
            added_workers: result
                .added_workers()
                .iter()
                .map(ToString::to_string)
                .collect(),
            removed_workers: result
                .removed_workers()
                .iter()
                .map(ToString::to_string)
                .collect(),
        })
    }

    fn destroy(&self) -> Result<(), String> {
        self.repository()?
            .worker_pool()
            .destroy()
            .map_err(|error| error.to_string())
    }

    fn discover_ready(&self, label: &str) -> Result<ReadyTicketResult, String> {
        let repository = self.repository()?;
        let runtime = repository
            .worker_pool()
            .snapshot()
            .pool()
            .and_then(|pool| pool.agent_runtime())
            .unwrap_or(AgentRuntime::Claude);
        let status = TicketStatus::parse("Todo").map_err(|error| error.to_string())?;
        let filter = ReadyTicketFilter::new(label, status).map_err(|error| error.to_string())?;
        let discovery = TicketDiscovery::new(configured_ticket_query(&repository, runtime));
        let ready = discovery
            .ready_tickets(&filter)
            .map_err(|error| error.to_string())?;
        Ok(ReadyTicketResult::new(
            ready
                .tickets()
                .iter()
                .map(|ticket| ReadyTicket::new(ticket.id().to_string(), ticket.title().as_str()))
                .collect(),
            ready
                .diagnostics()
                .iter()
                .map(|diagnostic| format!("{}: {}", diagnostic.subject(), diagnostic.reason()))
                .collect(),
        ))
    }

    fn orchestrate(
        &self,
        parent: &str,
        emit: &mut dyn FnMut(WorkspaceDispatchOrchestrationEvent),
    ) -> Result<(), String> {
        let repository = self.repository()?;
        let id = TicketId::parse(parent.to_owned()).map_err(|error| error.to_string())?;
        let runtime = repository
            .worker_pool()
            .snapshot()
            .pool()
            .and_then(|pool| pool.agent_runtime())
            .unwrap_or(AgentRuntime::Claude);
        let request = wsg_core::OrchestrationRequest::new(id.clone(), runtime);
        let ticket = DirectDispatchRequest::for_ticket_id(id.clone(), RunMode::Background)
            .map_err(|error| error.to_string())?
            .ticket()
            .clone();
        let parent_ticket = wsg_core::ParentTicket::new(ticket.id().clone());
        let discovery = TicketDiscovery::new(configured_ticket_query(&repository, runtime));
        let runner = repository.orchestration_runner();
        let preparation = runner
            .prepare(&request, &parent_ticket, &discovery)
            .map_err(|error| error.to_string())?;
        emit(WorkspaceDispatchOrchestrationEvent::Started {
            parent: id.to_string(),
            resumed: preparation.resumed(),
        });
        match preparation.into_start() {
            wsg_core::OrchestrationStart::Direct(success) => {
                let wsg_core::DirectDispatchExecution::Background { pid } = success.execution()
                else {
                    return Err("foreground Parent Dispatch is not supported by jjfx".to_owned());
                };
                emit(WorkspaceDispatchOrchestrationEvent::Direct {
                    parent: id.to_string(),
                    worker: success.worker().to_string(),
                    pid: *pid,
                });
                return Ok(());
            }
            wsg_core::OrchestrationStart::Group => {}
        }

        let mut projection_error = None;
        let options = wsg_core::OrchestrationOptions::new();
        runner
            .run(&request, &options, |event| {
                if matches!(event, wsg_core::OrchestrationEvent::Started { .. }) {
                    return;
                }
                let notice = orchestration_notice(&event);
                let progress = match repository.state_store().dispatch_group(id.clone()).load() {
                    Ok(wsg_core::Loaded::Present(versioned)) => {
                        DispatchGroupProgress::from_state(versioned.value)
                    }
                    Ok(wsg_core::Loaded::Missing) => Err(format!(
                        "Dispatch Group {id} disappeared during orchestration"
                    )),
                    Err(error) => Err(error.to_string()),
                };
                match progress {
                    Ok(progress) => {
                        emit(WorkspaceDispatchOrchestrationEvent::Progress { progress, notice })
                    }
                    Err(error) => projection_error = Some(error),
                }
                if let wsg_core::OrchestrationEvent::Terminal(summary) = event {
                    emit(WorkspaceDispatchOrchestrationEvent::Terminal {
                        parent: summary.parent().to_string(),
                        counts: summary.counts(),
                    });
                }
            })
            .map_err(|error| error.to_string())?;
        projection_error.map_or(Ok(()), Err)
    }

    fn dispatch(
        &self,
        tickets: &[String],
        worker: Option<&str>,
    ) -> Result<DispatchAdapterResult, WorkspaceDispatchError> {
        self.dispatch_with_strategy(tickets, worker, DispatchStrategy::Complete)
    }

    fn dispatch_with_approved_growth(
        &self,
        tickets: &[String],
        worker: Option<&str>,
        additional: usize,
    ) -> Result<DispatchAdapterResult, WorkspaceDispatchError> {
        self.dispatch_with_strategy(
            tickets,
            worker,
            DispatchStrategy::ApprovedGrowth(additional),
        )
    }

    fn dispatch_use_available(
        &self,
        tickets: &[String],
        worker: Option<&str>,
    ) -> Result<DispatchAdapterResult, WorkspaceDispatchError> {
        self.dispatch_with_strategy(tickets, worker, DispatchStrategy::Available)
    }

    fn send(&self, worker: &str, prompt: &str) -> Result<WorkerSessionOutcome, String> {
        self.worker_action(worker, WorkerActionKind::Send, Some(prompt))
    }

    fn review(&self, worker: &str) -> Result<WorkerSessionOutcome, String> {
        self.worker_action(worker, WorkerActionKind::Review, None)
    }

    fn reset(&self, worker: &str) -> Result<ResetAdapterResult, String> {
        let repository = self.repository()?;
        let worker_id = WorkerId::parse(worker.to_owned()).map_err(|error| error.to_string())?;
        let outcome = wsg_core::WorkerActions::new(repository)
            .reset(&worker_id)
            .map_err(|error| error.to_string())?;
        Ok(ResetAdapterResult::new(
            WorkerResetOutcome::new(worker, outcome.run()),
            outcome.into_restoration(),
        ))
    }

    fn rebase(&self, worker: &str) -> Result<WorkerCommandResult, String> {
        let repository = self.repository()?;
        let worker_id = WorkerId::parse(worker.to_owned()).map_err(|error| error.to_string())?;
        let outcome = wsg_core::WorkerActions::new(repository)
            .rebase(&worker_id)
            .map_err(|error| error.to_string())?;
        Ok(WorkerCommandResult::Rebased {
            worker: worker.to_owned(),
            branch: outcome.branch().to_owned(),
        })
    }

    fn open_pull_request(&self, worker: &str) -> Result<WorkerCommandResult, String> {
        let repository = self.repository()?;
        let worker_id = WorkerId::parse(worker.to_owned()).map_err(|error| error.to_string())?;
        let outcome = wsg_core::WorkerActions::new(repository)
            .open_pull_request(&worker_id)
            .map_err(|error| error.to_string())?;
        Ok(WorkerCommandResult::PullRequestOpened {
            worker: worker.to_owned(),
            branch: outcome.branch().to_owned(),
        })
    }

    fn set_alias(&self, worker: &str, alias: &str) -> Result<WorkerCommandResult, String> {
        let repository = self.repository()?;
        let worker_id = WorkerId::parse(worker.to_owned()).map_err(|error| error.to_string())?;
        let alias = match alias.trim() {
            "" => None,
            value => Some(value.to_owned()),
        };
        repository
            .worker_pool()
            .set_alias(worker_id, alias.clone().unwrap_or_default())
            .map_err(|error| error.to_string())?;
        Ok(WorkerCommandResult::AliasChanged {
            worker: worker.to_owned(),
            alias,
        })
    }

    fn dismiss(&self, worker: &str) -> Result<WorkerCommandResult, String> {
        let repository = self.repository()?;
        let worker_id = WorkerId::parse(worker.to_owned()).map_err(|error| error.to_string())?;
        let outcome = wsg_core::WorkerActions::new(repository)
            .dismiss(&worker_id)
            .map_err(|error| error.to_string())?;
        let disposition = match outcome {
            wsg_core::DismissOutcome::Removed { capacity } => {
                WorkerDismissDisposition::Removed { capacity }
            }
            wsg_core::DismissOutcome::Reset => WorkerDismissDisposition::Reset,
        };
        Ok(WorkerCommandResult::Dismissed {
            worker: worker.to_owned(),
            disposition,
        })
    }

    fn worker_log(&self, worker: &str) -> Result<WorkerLogSnapshot, String> {
        let repository = self.repository()?;
        let worker_id = WorkerId::parse(worker.to_owned()).map_err(|error| error.to_string())?;
        let snapshot = repository.worker_pool().snapshot();
        let state = snapshot
            .worker(worker)
            .ok_or_else(|| format!("Worker {worker} was not found"))?;
        let runtime = state.agent_runtime().unwrap_or(AgentRuntime::Claude);
        let logs = wsg_core::WorkerActions::new(repository)
            .logs(&worker_id)
            .map_err(|error| error.to_string())?;
        let log = logs.open();
        let activity = log.current_activity().map_err(|error| error.to_string())?;
        let result = match state.status() {
            WorkerStatus::Done | WorkerStatus::Failed => {
                log.final_result().map_err(|error| error.to_string())?
            }
            WorkerStatus::Idle | WorkerStatus::Busy => None,
        };
        Ok(WorkerLogSnapshot::new(
            worker.to_owned(),
            runtime,
            activity,
            result,
        ))
    }
}

/// Test adapter that records commands while returning a supplied snapshot.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct RecordingAdapter {
    commands: Arc<std::sync::Mutex<Vec<WorkspaceDispatchCommand>>>,
    snapshot: WorkerPoolSnapshot,
    log_snapshot: WorkerLogSnapshot,
}

#[cfg(test)]
impl RecordingAdapter {
    pub(crate) fn new(snapshot: WorkerPoolSnapshot) -> Self {
        Self {
            commands: Arc::new(std::sync::Mutex::new(Vec::new())),
            snapshot,
            log_snapshot: WorkerLogSnapshot::new("worker-01", AgentRuntime::Claude, None, None),
        }
    }

    pub(crate) fn with_log_snapshot(mut self, snapshot: WorkerLogSnapshot) -> Self {
        self.log_snapshot = snapshot;
        self
    }

    pub(crate) fn commands(&self) -> Vec<WorkspaceDispatchCommand> {
        self.commands
            .lock()
            .expect("recording adapter lock")
            .clone()
    }

    fn result(tickets: &[String], worker: Option<&str>) -> DispatchAdapterResult {
        DispatchAdapterResult::new(DispatchResult {
            outcomes: tickets
                .iter()
                .map(|ticket| DispatchOutcome {
                    ticket: ticket.clone(),
                    title: ticket.clone(),
                    worker: worker.map(str::to_owned),
                    pid: Some(42),
                    phase: None,
                    detail: None,
                })
                .collect(),
            partial: false,
        })
    }
}

#[cfg(test)]
impl WorkspaceDispatchAdapter for RecordingAdapter {
    fn refresh(&self) -> Result<WorkerPoolSnapshot, String> {
        self.commands
            .lock()
            .expect("recording adapter lock")
            .push(WorkspaceDispatchCommand::Refresh { operation: 0 });
        Ok(self.snapshot.clone())
    }

    fn resize(&self, capacity: usize) -> Result<PoolMutationResult, String> {
        self.commands.lock().expect("recording adapter lock").push(
            WorkspaceDispatchCommand::Resize {
                operation: 0,
                capacity,
            },
        );
        Ok(PoolMutationResult {
            capacity,
            added_workers: Vec::new(),
            removed_workers: Vec::new(),
        })
    }

    fn destroy(&self) -> Result<(), String> {
        self.commands
            .lock()
            .expect("recording adapter lock")
            .push(WorkspaceDispatchCommand::Destroy { operation: 0 });
        Ok(())
    }

    fn dispatch(
        &self,
        tickets: &[String],
        worker: Option<&str>,
    ) -> Result<DispatchAdapterResult, WorkspaceDispatchError> {
        self.commands.lock().expect("recording adapter lock").push(
            WorkspaceDispatchCommand::Dispatch {
                operation: 0,
                tickets: tickets.to_vec(),
                worker: worker.map(str::to_owned),
            },
        );
        Ok(Self::result(tickets, worker))
    }

    fn dispatch_with_approved_growth(
        &self,
        tickets: &[String],
        worker: Option<&str>,
        additional: usize,
    ) -> Result<DispatchAdapterResult, WorkspaceDispatchError> {
        self.commands.lock().expect("recording adapter lock").push(
            WorkspaceDispatchCommand::DispatchWithApprovedGrowth {
                operation: 0,
                tickets: tickets.to_vec(),
                worker: worker.map(str::to_owned),
                additional,
            },
        );
        Ok(Self::result(tickets, worker))
    }

    fn dispatch_use_available(
        &self,
        tickets: &[String],
        worker: Option<&str>,
    ) -> Result<DispatchAdapterResult, WorkspaceDispatchError> {
        self.commands.lock().expect("recording adapter lock").push(
            WorkspaceDispatchCommand::DispatchUseAvailable {
                operation: 0,
                tickets: tickets.to_vec(),
                worker: worker.map(str::to_owned),
            },
        );
        Ok(Self::result(tickets, worker))
    }

    fn discover_ready(&self, _label: &str) -> Result<ReadyTicketResult, String> {
        self.commands.lock().expect("recording adapter lock").push(
            WorkspaceDispatchCommand::DiscoverReady {
                operation: 0,
                label: _label.to_owned(),
            },
        );
        Ok(ReadyTicketResult {
            tickets: vec![ReadyTicket {
                id: "ENG-42".to_owned(),
                title: "Example ready Ticket".to_owned(),
            }],
            diagnostics: Vec::new(),
        })
    }

    fn orchestrate(
        &self,
        parent: &str,
        emit: &mut dyn FnMut(WorkspaceDispatchOrchestrationEvent),
    ) -> Result<(), String> {
        self.commands.lock().expect("recording adapter lock").push(
            WorkspaceDispatchCommand::Orchestrate {
                operation: 0,
                parent: parent.to_owned(),
            },
        );
        let mut state = wsg_core::DispatchGroupState::new(
            wsg_core::TicketId::parse(parent.to_owned()).map_err(|error| error.to_string())?,
            wsg_core::WireTimestamp::new("2026-08-10T10:00:00Z"),
            "Jarvvski/jjfx",
            wsg_core::DispatchGroupOptions::new(""),
        );
        state.sub_issues = std::collections::BTreeMap::new();
        let progress = DispatchGroupProgress::from_state(state)?;
        emit(WorkspaceDispatchOrchestrationEvent::Started {
            parent: parent.to_owned(),
            resumed: true,
        });
        emit(WorkspaceDispatchOrchestrationEvent::Progress {
            progress,
            notice: Some("orchestration test update".to_owned()),
        });
        emit(WorkspaceDispatchOrchestrationEvent::Terminal {
            parent: parent.to_owned(),
            counts: DispatchGroupStatusCounts::default(),
        });
        Ok(())
    }

    fn send(&self, worker: &str, prompt: &str) -> Result<WorkerSessionOutcome, String> {
        self.commands.lock().expect("recording adapter lock").push(
            WorkspaceDispatchCommand::Send {
                operation: 0,
                worker: worker.to_owned(),
                prompt: prompt.to_owned(),
            },
        );
        Ok(WorkerSessionOutcome::new(
            worker,
            WorkerActionKind::Send,
            AgentRuntime::Claude,
            AgentSessionResolution::Fresh {
                reason: wsg_core::FreshSessionReason::NoPriorLog,
            },
            42,
        ))
    }

    fn review(&self, worker: &str) -> Result<WorkerSessionOutcome, String> {
        self.commands.lock().expect("recording adapter lock").push(
            WorkspaceDispatchCommand::Review {
                operation: 0,
                worker: worker.to_owned(),
            },
        );
        Ok(WorkerSessionOutcome::new(
            worker,
            WorkerActionKind::Review,
            AgentRuntime::Claude,
            AgentSessionResolution::Resumed {
                session_id: "session-42".to_owned(),
            },
            43,
        ))
    }

    fn reset(&self, worker: &str) -> Result<ResetAdapterResult, String> {
        self.commands.lock().expect("recording adapter lock").push(
            WorkspaceDispatchCommand::Reset {
                operation: 0,
                worker: worker.to_owned(),
            },
        );
        Ok(ResetAdapterResult::new(
            WorkerResetOutcome::new(worker, wsg_core::RunReset::AlreadyIdle),
            wsg_core::WorkspaceRestoration::SkippedMissingWorkspace,
        ))
    }

    fn rebase(&self, worker: &str) -> Result<WorkerCommandResult, String> {
        self.commands.lock().expect("recording adapter lock").push(
            WorkspaceDispatchCommand::Rebase {
                operation: 0,
                worker: worker.to_owned(),
            },
        );
        Ok(WorkerCommandResult::Rebased {
            worker: worker.to_owned(),
            branch: "main".to_owned(),
        })
    }

    fn open_pull_request(&self, worker: &str) -> Result<WorkerCommandResult, String> {
        self.commands.lock().expect("recording adapter lock").push(
            WorkspaceDispatchCommand::OpenPullRequest {
                operation: 0,
                worker: worker.to_owned(),
            },
        );
        Ok(WorkerCommandResult::PullRequestOpened {
            worker: worker.to_owned(),
            branch: "main".to_owned(),
        })
    }

    fn set_alias(&self, worker: &str, alias: &str) -> Result<WorkerCommandResult, String> {
        self.commands.lock().expect("recording adapter lock").push(
            WorkspaceDispatchCommand::SetAlias {
                operation: 0,
                worker: worker.to_owned(),
                alias: alias.to_owned(),
            },
        );
        Ok(WorkerCommandResult::AliasChanged {
            worker: worker.to_owned(),
            alias: (!alias.trim().is_empty()).then(|| alias.trim().to_owned()),
        })
    }

    fn dismiss(&self, worker: &str) -> Result<WorkerCommandResult, String> {
        self.commands.lock().expect("recording adapter lock").push(
            WorkspaceDispatchCommand::Dismiss {
                operation: 0,
                worker: worker.to_owned(),
            },
        );
        Ok(WorkerCommandResult::Dismissed {
            worker: worker.to_owned(),
            disposition: WorkerDismissDisposition::Reset,
        })
    }

    fn worker_log(&self, worker: &str) -> Result<WorkerLogSnapshot, String> {
        self.commands.lock().expect("recording adapter lock").push(
            WorkspaceDispatchCommand::WatchWorkerLog {
                operation: 0,
                worker: worker.to_owned(),
            },
        );
        Ok(self.log_snapshot.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use tempfile::TempDir;

    const PI_HELPER_REPOSITORY: &str = "JJFX_TEST_PI_HELPER_REPOSITORY";
    const PI_HELPER_RESULT: &str = "JJFX_TEST_PI_HELPER_RESULT";

    fn empty_snapshot() -> WorkerPoolSnapshot {
        let directory = TempDir::new().expect("temporary repository");
        let output = Command::new("jj")
            .args(["--config", "signing.behavior=drop", "git", "init"])
            .arg(directory.path())
            .output()
            .expect("jj should be installed");
        assert!(output.status.success());
        wsg_core::Repository::open(directory.path())
            .expect("repository should open")
            .read_worker_pool_snapshot()
    }

    fn pi_repository() -> (TempDir, wsg_core::Repository) {
        let directory = TempDir::new().expect("temporary repository");
        let output = Command::new("jj")
            .args(["--config", "signing.behavior=drop", "git", "init"])
            .arg(directory.path())
            .output()
            .expect("jj should be installed");
        assert!(output.status.success());
        let repository = wsg_core::Repository::open(directory.path()).expect("repository opens");
        repository
            .worker_pool()
            .resize_to(PoolCapacity::new(1).expect("Pool capacity"))
            .expect("Worker Pool grows");
        let state_repository = repository.state_store().pool();
        let loaded = match state_repository.load().expect("Pool state") {
            wsg_core::Loaded::Present(versioned) => versioned,
            wsg_core::Loaded::Missing => panic!("Pool state should exist"),
        };
        let (mut state, revision) = loaded.into_parts();
        state.agent = Some(wsg_core::WireAgent::new(AgentRuntime::Pi.as_str()));
        let outcome = state_repository
            .commit(
                wsg_core::Expected::Match(revision),
                wsg_core::StateChange::Replace(state),
            )
            .expect("configured Pool runtime");
        assert!(matches!(outcome, wsg_core::CommitOutcome::Applied(_)));
        (directory, repository)
    }

    fn write_executable(path: &std::path::Path, body: &str) {
        fs::write(path, body).expect("write helper executable");
        let mut permissions = fs::metadata(path).expect("helper metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("helper permissions");
    }

    #[test]
    fn real_workspace_dispatch_uses_pi_helper_configuration_before_pool_mutation() {
        let (directory, repository) = pi_repository();
        let helper = directory.path().join("pi-linear-helper");
        let request = directory.path().join("pi-linear-request.json");
        let result = directory.path().join("pi-linear-result");
        write_executable(
            &helper,
            &format!(
                "#!/bin/sh\ncat > '{}'\nprintf '%s\\n' '{{\"version\":1,\"result\":{{\"tickets\":[]}}}}'\n",
                request.display(),
            ),
        );

        let output = Command::new(env::current_exe().expect("test executable"))
            .args(["real_workspace_dispatch_pi_helper", "--ignored"])
            .env(PI_HELPER_REPOSITORY, repository.root())
            .env(PI_HELPER_RESULT, &result)
            .env("JJFX_PI_LINEAR_HELPER", &helper)
            .output()
            .expect("TUI adapter helper should run");

        assert!(
            output.status.success(),
            "TUI adapter helper failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read_to_string(result).expect("adapter result"), "0");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &fs::read_to_string(request).expect("captured helper request")
            )
            .expect("valid helper request"),
            serde_json::json!({
                "version": 1,
                "operation": "ready_tickets",
                "label": "ready-for-agent",
                "status": "Todo",
            }),
        );
        let snapshot = repository.worker_pool().snapshot();
        assert!(
            snapshot.workers().iter().all(|worker| {
                worker.status() == WorkerStatus::Idle && worker.ticket().is_none()
            })
        );
    }

    #[test]
    #[ignore]
    fn real_workspace_dispatch_pi_helper() {
        let adapter = RealWorkspaceDispatch::new(
            env::var_os(PI_HELPER_REPOSITORY).expect("helper repository"),
        );
        let ready = adapter
            .discover_ready("ready-for-agent")
            .expect("Pi TUI discovery");
        fs::write(
            env::var_os(PI_HELPER_RESULT).expect("helper result"),
            ready.tickets().len().to_string(),
        )
        .expect("write helper result");
    }

    #[test]
    fn controller_dispatches_tickets_to_the_selected_worker() {
        let adapter = RecordingAdapter::new(empty_snapshot());
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let controller = WorkspaceDispatchController::new(adapter.clone(), move |event| {
            events_tx.send(event).expect("event receiver");
        });

        controller.submit(WorkspaceDispatchCommand::Dispatch {
            operation: 9,
            tickets: vec!["ENG-42".to_owned()],
            worker: Some("worker-01".to_owned()),
        });

        assert!(matches!(
            events_rx.recv().expect("dispatch event"),
            WorkspaceDispatchEvent::Dispatched { operation: 9, result }
                if result.outcomes()[0].ticket() == "ENG-42"
                    && result.outcomes()[0].worker() == Some("worker-01")
        ));
        assert!(adapter.commands().iter().any(|command| matches!(
            command,
            WorkspaceDispatchCommand::Dispatch {
                operation: 0,
                tickets,
                worker: Some(worker),
            } if tickets == &["ENG-42".to_owned()] && worker == "worker-01"
        )));
    }

    #[test]
    fn controller_preserves_approved_capacity_and_available_dispatch_commands() {
        let adapter = RecordingAdapter::new(empty_snapshot());
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let controller = WorkspaceDispatchController::new(adapter.clone(), move |event| {
            events_tx.send(event).expect("event receiver");
        });
        let tickets = vec!["ENG-42".to_owned(), "ENG-43".to_owned()];

        controller.submit(WorkspaceDispatchCommand::DispatchWithApprovedGrowth {
            operation: 12,
            tickets: tickets.clone(),
            worker: None,
            additional: 1,
        });
        assert!(matches!(
            events_rx.recv().expect("growth event"),
            WorkspaceDispatchEvent::Dispatched { operation: 12, result }
                if result.outcomes().len() == 2
        ));
        controller.submit(WorkspaceDispatchCommand::DispatchUseAvailable {
            operation: 13,
            tickets: tickets.clone(),
            worker: None,
        });
        assert!(matches!(
            events_rx.recv().expect("available event"),
            WorkspaceDispatchEvent::Dispatched { operation: 13, .. }
        ));
        let commands = adapter.commands();
        assert!(commands.iter().any(|command| matches!(
            command,
            WorkspaceDispatchCommand::DispatchWithApprovedGrowth {
                additional: 1,
                tickets: command_tickets,
                ..
            } if command_tickets == &tickets
        )));
        assert!(commands.iter().any(|command| matches!(
            command,
            WorkspaceDispatchCommand::DispatchUseAvailable {
                tickets: command_tickets,
                ..
            } if command_tickets == &tickets
        )));
    }

    #[test]
    fn controller_discovers_ready_tickets_in_provider_order() {
        let adapter = RecordingAdapter::new(empty_snapshot());
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let controller = WorkspaceDispatchController::new(adapter.clone(), move |event| {
            events_tx.send(event).expect("event receiver");
        });

        controller.submit(WorkspaceDispatchCommand::DiscoverReady {
            operation: 11,
            label: "ready-for-agent".to_owned(),
        });

        assert!(matches!(
            events_rx.recv().expect("discovery event"),
            WorkspaceDispatchEvent::ReadyTickets { operation: 11, result }
                if result.tickets()[0].id() == "ENG-42"
                    && result.tickets()[0].title() == "Example ready Ticket"
        ));
        assert!(adapter.commands().iter().any(|command| matches!(
            command,
            WorkspaceDispatchCommand::DiscoverReady { operation: 0, label }
                if label == "ready-for-agent"
        )));
    }

    #[test]
    fn controller_streams_ordered_orchestration_updates_without_waiting_for_app() {
        let adapter = RecordingAdapter::new(empty_snapshot());
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let controller = WorkspaceDispatchController::new(adapter.clone(), move |event| {
            events_tx.send(event).expect("event receiver");
        });

        controller.submit(WorkspaceDispatchCommand::Orchestrate {
            operation: 22,
            parent: "ENG-100".to_owned(),
        });

        assert!(matches!(
            events_rx.recv().expect("started event"),
            WorkspaceDispatchEvent::OrchestrationStarted {
                operation: 22,
                parent,
                resumed: true,
            } if parent == "ENG-100"
        ));
        assert!(matches!(
            events_rx.recv().expect("progress event"),
            WorkspaceDispatchEvent::OrchestrationProgress {
                operation: 22,
                progress,
                notice: Some(notice),
            } if progress.parent() == "ENG-100" && notice == "orchestration test update"
        ));
        assert!(matches!(
            events_rx.recv().expect("terminal event"),
            WorkspaceDispatchEvent::OrchestrationTerminal {
                operation: 22,
                parent,
                counts,
            } if parent == "ENG-100" && counts == DispatchGroupStatusCounts::default()
        ));
    }

    #[test]
    fn controller_launches_follow_up_actions_with_session_outcomes() {
        let adapter = RecordingAdapter::new(empty_snapshot());
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let controller = WorkspaceDispatchController::new(adapter.clone(), move |event| {
            events_tx.send(event).expect("event receiver");
        });

        controller.submit(WorkspaceDispatchCommand::Send {
            operation: 40,
            worker: "worker-01".to_owned(),
            prompt: "continue the work".to_owned(),
        });
        assert!(matches!(
            events_rx.recv().expect("send event"),
            WorkspaceDispatchEvent::WorkerActionCompleted { operation: 40, outcome }
                if outcome.worker() == "worker-01"
                    && outcome.action() == WorkerActionKind::Send
                    && matches!(
                        outcome.session(),
                        AgentSessionResolution::Fresh {
                            reason: wsg_core::FreshSessionReason::NoPriorLog
                        }
                    )
        ));

        controller.submit(WorkspaceDispatchCommand::Review {
            operation: 41,
            worker: "worker-01".to_owned(),
        });
        assert!(matches!(
            events_rx.recv().expect("review event"),
            WorkspaceDispatchEvent::WorkerActionCompleted { operation: 41, outcome }
                if outcome.worker() == "worker-01"
                    && outcome.action() == WorkerActionKind::Review
                    && matches!(
                        outcome.session(),
                        AgentSessionResolution::Resumed { session_id }
                            if session_id == "session-42"
                    )
        ));
        let commands = adapter.commands();
        assert!(commands.iter().any(|command| matches!(
            command,
            WorkspaceDispatchCommand::Send { worker, prompt, .. }
                if worker == "worker-01" && prompt == "continue the work"
        )));
        assert!(commands.iter().any(|command| matches!(
            command,
            WorkspaceDispatchCommand::Review { worker, .. } if worker == "worker-01"
        )));
    }

    #[test]
    fn controller_completes_rebase_pull_request_alias_and_dismiss_actions() {
        let adapter = RecordingAdapter::new(empty_snapshot());
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let controller = WorkspaceDispatchController::new(adapter.clone(), move |event| {
            events_tx.send(event).expect("event receiver");
        });

        controller.submit(WorkspaceDispatchCommand::Rebase {
            operation: 50,
            worker: "worker-01".to_owned(),
        });
        assert!(matches!(
            events_rx.recv().expect("rebase event"),
            WorkspaceDispatchEvent::WorkerCommandCompleted {
                operation: 50,
                result: WorkerCommandResult::Rebased { worker, branch },
            } if worker == "worker-01" && branch == "main"
        ));

        controller.submit(WorkspaceDispatchCommand::OpenPullRequest {
            operation: 51,
            worker: "worker-01".to_owned(),
        });
        assert!(matches!(
            events_rx.recv().expect("pull request event"),
            WorkspaceDispatchEvent::WorkerCommandCompleted {
                operation: 51,
                result: WorkerCommandResult::PullRequestOpened { worker, branch },
            } if worker == "worker-01" && branch == "main"
        ));

        controller.submit(WorkspaceDispatchCommand::SetAlias {
            operation: 52,
            worker: "worker-01".to_owned(),
            alias: "  primary  ".to_owned(),
        });
        assert!(matches!(
            events_rx.recv().expect("alias event"),
            WorkspaceDispatchEvent::WorkerCommandCompleted {
                operation: 52,
                result: WorkerCommandResult::AliasChanged { worker, alias },
            } if worker == "worker-01" && alias.as_deref() == Some("primary")
        ));

        controller.submit(WorkspaceDispatchCommand::Dismiss {
            operation: 53,
            worker: "worker-01".to_owned(),
        });
        assert!(matches!(
            events_rx.recv().expect("dismiss event"),
            WorkspaceDispatchEvent::WorkerCommandCompleted {
                operation: 53,
                result: WorkerCommandResult::Dismissed { worker, disposition },
            } if worker == "worker-01" && disposition == WorkerDismissDisposition::Reset
        ));
        let commands = adapter.commands();
        assert!(commands.iter().any(|command| matches!(
            command,
            WorkspaceDispatchCommand::Rebase { worker, .. } if worker == "worker-01"
        )));
        assert!(commands.iter().any(|command| matches!(
            command,
            WorkspaceDispatchCommand::OpenPullRequest { worker, .. } if worker == "worker-01"
        )));
        assert!(commands.iter().any(|command| matches!(
            command,
            WorkspaceDispatchCommand::SetAlias { worker, alias, .. }
                if worker == "worker-01" && alias == "  primary  "
        )));
        assert!(commands.iter().any(|command| matches!(
            command,
            WorkspaceDispatchCommand::Dismiss { worker, .. } if worker == "worker-01"
        )));
    }

    #[test]
    fn controller_reports_reset_before_independent_restoration_completion() {
        let adapter = RecordingAdapter::new(empty_snapshot());
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let controller = WorkspaceDispatchController::new(adapter.clone(), move |event| {
            events_tx.send(event).expect("event receiver");
        });

        controller.submit(WorkspaceDispatchCommand::Reset {
            operation: 42,
            worker: "worker-01".to_owned(),
        });

        assert!(matches!(
            events_rx.recv().expect("reset event"),
            WorkspaceDispatchEvent::WorkerResetCompleted { operation: 42, outcome }
                if outcome.worker() == "worker-01"
                    && outcome.run() == wsg_core::RunReset::AlreadyIdle
        ));
        assert!(matches!(
            events_rx.recv().expect("refresh event"),
            WorkspaceDispatchEvent::Snapshot { operation: 42, .. }
        ));
        assert!(matches!(
            events_rx.recv().expect("restoration event"),
            WorkspaceDispatchEvent::WorkspaceRestorationCompleted {
                operation: 42,
                worker,
                result: WorkspaceRestorationResult::Skipped,
            } if worker == "worker-01"
        ));
        assert!(adapter.commands().iter().any(|command| matches!(
            command,
            WorkspaceDispatchCommand::Reset { worker, .. } if worker == "worker-01"
        )));
    }

    #[test]
    fn worker_log_watch_deduplicates_activity_and_stops_on_request() {
        let activity = RunActivity::new(wsg_core::RunActivityKind::Message {
            text: "hello from Claude".to_owned(),
        });
        let adapter = RecordingAdapter::new(empty_snapshot()).with_log_snapshot(
            WorkerLogSnapshot::new("worker-01", AgentRuntime::Claude, Some(activity), None),
        );
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let controller = WorkspaceDispatchController::new(adapter.clone(), move |event| {
            events_tx.send(event).expect("event receiver");
        });

        controller.submit(WorkspaceDispatchCommand::WatchWorkerLog {
            operation: 31,
            worker: "worker-01".to_owned(),
        });
        assert!(matches!(
            events_rx.recv().expect("first log update"),
            WorkspaceDispatchEvent::WorkerLogUpdated { operation: 31, snapshot }
                if snapshot.worker() == "worker-01"
                    && snapshot.activity().is_some_and(|activity| matches!(
                        activity.kind(),
                        wsg_core::RunActivityKind::Message { text } if text == "hello from Claude"
                    ))
        ));
        assert!(
            events_rx
                .recv_timeout(std::time::Duration::from_millis(250))
                .is_err()
        );
        controller.submit(WorkspaceDispatchCommand::StopWorkerLog { operation: 31 });
        assert!(adapter.commands().iter().any(|command| {
            matches!(
                command,
                WorkspaceDispatchCommand::WatchWorkerLog { worker, .. }
                    if worker == "worker-01"
            )
        }));
    }

    #[test]
    fn controller_emits_a_snapshot_for_a_refresh_command() {
        let adapter = RecordingAdapter::new(empty_snapshot());
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let controller = WorkspaceDispatchController::new(adapter.clone(), move |event| {
            events_tx.send(event).expect("event receiver");
        });

        controller.submit(WorkspaceDispatchCommand::Refresh { operation: 7 });

        assert!(matches!(
            events_rx.recv().expect("refresh event"),
            WorkspaceDispatchEvent::Snapshot { operation: 7, .. }
        ));
        assert!(
            adapter.commands().iter().any(|command| matches!(
                command,
                WorkspaceDispatchCommand::Refresh { operation: 0 }
            ))
        );
    }

    #[test]
    fn dispatch_group_progress_projects_dependency_waves_and_ready_tickets() {
        use std::collections::BTreeMap;
        use wsg_core::{DispatchGroupState, SubIssueState, TicketId, WireStatus, WireTimestamp};

        let parent = TicketId::parse("ENG-100").expect("parent Ticket");
        let blocker = TicketId::parse("ENG-101").expect("blocker Ticket");
        let dependent = TicketId::parse("ENG-102").expect("dependent Ticket");
        let leaf = TicketId::parse("ENG-103").expect("leaf Ticket");
        let independent = TicketId::parse("ENG-104").expect("independent Ticket");
        let mut sub_issues = BTreeMap::new();
        sub_issues.insert(
            blocker.clone(),
            SubIssueState::new("Foundation", WireStatus::new("done"), Vec::new()),
        );
        sub_issues.insert(
            dependent.clone(),
            SubIssueState::new(
                "Dependent work",
                WireStatus::new("pending"),
                vec![blocker.clone()],
            ),
        );
        sub_issues.insert(
            leaf.clone(),
            SubIssueState::new(
                "Leaf work",
                WireStatus::new("pending"),
                vec![dependent.clone()],
            ),
        );
        let mut assigned = SubIssueState::new(
            "Independent work",
            WireStatus::new("dispatched"),
            Vec::new(),
        );
        assigned.worker = Some(wsg_core::WorkerId::parse("worker-01").expect("Worker"));
        assigned.dispatched_at = Some(WireTimestamp::new("2026-08-10T10:01:00Z"));
        assigned.retries = 1;
        sub_issues.insert(independent.clone(), assigned);

        let mut state = DispatchGroupState::new(
            parent,
            WireTimestamp::new("2026-08-10T10:00:00Z"),
            "Jarvvski/jjfx",
            wsg_core::DispatchGroupOptions::new(""),
        );
        state.sub_issues = sub_issues;

        let progress = DispatchGroupProgress::from_state(state).expect("valid progress");

        assert_eq!(progress.parent(), "ENG-100");
        assert_eq!(progress.maximum_wave(), 2);
        assert_eq!(progress.ready(), &["ENG-102"]);
        assert_eq!(progress.issues()[0].ticket(), "ENG-101");
        assert_eq!(progress.issues()[1].wave(), 2);
        assert_eq!(progress.issues()[2].wave(), 3);
        assert_eq!(
            progress.issues()[3].status(),
            wsg_core::SubIssueStatus::Dispatched
        );
        assert_eq!(progress.issues()[3].worker(), Some("worker-01"));
        assert_eq!(progress.issues()[3].retries(), 1);
    }
}
