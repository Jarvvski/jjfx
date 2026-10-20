//! The Workspace Dispatch command/event seam used by the jjfx App.
//!
//! The App submits a small command vocabulary and receives immutable events.
//! The blocking port and its production adapter live in [`adapter`].

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::engine::{
    AgentRuntime, AgentSessionResolution, DirectDispatchError, DirectDispatchExecution,
    DirectDispatchFailurePhase, DirectDispatchOutcome, DirectDispatchRequest,
    DispatchGroupProgress, DispatchGroupStatusCounts, MutationKind, PermissionDecision,
    PoolCapacity, PoolName, ReadyTicketFilter, RunActivity, RunMode, RunResult, TicketDiscovery,
    TicketId, TicketStatus, WorkerId, WorkerPoolError, WorkerPoolSnapshot, WorkerStatus,
};

mod adapter;

pub(crate) use adapter::{RealWorkspaceDispatch, WorkspaceDispatchAdapter};

/// A user-visible operation identity used to ignore stale results.
pub type OperationId = u64;

/// The Worker Pool a TUI session acts on, shared between the App that names a
/// lane and the adapter that reads it for every pool-scoped operation.
///
/// The lane is chosen once at launch and may be changed while the session runs,
/// which is why it is a shared cell rather than a value captured at
/// construction: the App owns the choice for rendering and the background
/// controller - including the refresh poller - reads the same cell, so a lane
/// change reaches every operation without rebuilding the controller.
#[derive(Clone)]
pub struct PoolSelection {
    pool: Arc<Mutex<PoolName>>,
}

impl PoolSelection {
    /// Names the lane every operation acts on.
    pub fn new(pool: PoolName) -> Self {
        Self {
            pool: Arc::new(Mutex::new(pool)),
        }
    }

    /// Returns the lane in force right now.
    pub fn get(&self) -> PoolName {
        self.pool
            .lock()
            .expect("Worker Pool selection lock")
            .clone()
    }

    /// Replaces the lane every subsequent operation acts on.
    pub fn set(&self, pool: PoolName) {
        *self.pool.lock().expect("Worker Pool selection lock") = pool;
    }
}

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
    pid: Option<u32>,
}

impl WorkerSessionOutcome {
    pub(crate) fn new(
        worker: impl Into<String>,
        action: WorkerActionKind,
        runtime: AgentRuntime,
        session: AgentSessionResolution,
        pid: Option<u32>,
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

    /// Returns the Agent Runtime process identifier, or `None` when the Run was
    /// delivered to its session through the daemon API and drives no process.
    pub const fn pid(&self) -> Option<u32> {
        self.pid
    }
}

/// Immutable presentation of a Worker Run Reset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerResetOutcome {
    worker: String,
    run: crate::engine::RunReset,
}

impl WorkerResetOutcome {
    pub(crate) fn new(worker: impl Into<String>, run: crate::engine::RunReset) -> Self {
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
    pub const fn run(&self) -> crate::engine::RunReset {
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
    restoration: crate::engine::WorkspaceRestoration,
}

impl ResetAdapterResult {
    fn new(outcome: WorkerResetOutcome, restoration: crate::engine::WorkspaceRestoration) -> Self {
        Self {
            outcome,
            restoration,
        }
    }

    fn into_parts(self) -> (WorkerResetOutcome, crate::engine::WorkspaceRestoration) {
        (self.outcome, self.restoration)
    }
}

/// How a Dismiss operation changed Worker capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerDismissDisposition {
    /// The idle Worker was removed from its Worker Pool.
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
    /// A Worker's pending opencode permission request was answered.
    PermissionResolved {
        worker: String,
        action: String,
        decision: PermissionDecision,
    },
    /// The last jjfx-initiated mutation on a Worker was undone through jj's
    /// operation log.
    MutationUndone {
        worker: String,
        kind: MutationKind,
        discarded: usize,
        later: usize,
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
            Self::PermissionResolved {
                worker,
                action,
                decision,
            } => format!(
                "Answered {worker}'s {action} permission request ({})",
                decision.as_str()
            ),
            Self::MutationUndone {
                worker,
                kind,
                discarded,
                later,
            } => {
                let base = format!("Undid the last {kind} on {worker} ({discarded} operations)");
                if *later > 0 {
                    format!(
                        "{base}; the repository-wide restore also discarded {later} later operation(s)"
                    )
                } else {
                    base
                }
            }
        }
    }
}

/// Commands accepted by the Workspace Dispatch controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceDispatchCommand {
    /// Reconcile recorded Runs and read a complete immutable Pool snapshot.
    Refresh { operation: OperationId },
    /// Set the exact Worker Pool capacity, creating it when necessary.
    Resize {
        operation: OperationId,
        capacity: usize,
    },
    /// Destroy the Worker Pool after the caller has obtained confirmation.
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
    /// Discover provider-ready Tickets through the selected Worker Pool's own
    /// routing label and ready status.
    DiscoverReady { operation: OperationId },
    /// Start or resume one Parent Ticket's dependency-aware orchestration.
    Orchestrate {
        operation: OperationId,
        parent: String,
    },
    /// Send a prompt to one selected Worker, or the first idle Worker when no
    /// target is supplied.
    Send {
        operation: OperationId,
        worker: Option<String>,
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
    /// Answer one Worker's pending opencode permission request.
    ResolvePermission {
        operation: OperationId,
        worker: String,
        decision: PermissionDecision,
    },
    /// Undo the last jjfx-initiated mutation on one Worker.
    Undo {
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
    /// The opencode session the prompt was delivered to, when the Run drove no
    /// client process.
    session_id: Option<String>,
    phase: Option<String>,
    detail: Option<String>,
    /// The capacity governor held this Ticket back: nothing was attempted.
    waiting: bool,
}

impl DispatchOutcome {
    /// Creates a successful background-launch outcome.
    pub fn success(ticket: String, title: String, worker: String, pid: u32) -> Self {
        Self {
            ticket,
            title,
            worker: Some(worker),
            pid: Some(pid),
            session_id: None,
            phase: None,
            detail: None,
            waiting: false,
        }
    }

    /// Creates a successful outcome for a Run delivered to its opencode session
    /// with no client process.
    pub fn session(ticket: String, title: String, worker: String, session_id: String) -> Self {
        Self {
            ticket,
            title,
            worker: Some(worker),
            pid: None,
            session_id: Some(session_id),
            phase: None,
            detail: None,
            waiting: false,
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
            session_id: None,
            phase: Some(format!("{phase:?}")),
            detail: Some(detail),
            waiting: false,
        }
    }

    /// Creates an outcome for a Ticket the capacity governor held back.
    pub fn waiting(ticket: String, title: String, detail: String) -> Self {
        Self {
            ticket,
            title,
            worker: None,
            pid: None,
            session_id: None,
            phase: None,
            detail: Some(detail),
            waiting: true,
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
    /// Returns the opencode session the prompt was delivered to, when the Run
    /// drove no client process.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
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
        self.pid.is_some() || self.session_id.is_some()
    }
    /// Reports whether the capacity governor held the Ticket back.
    pub const fn is_waiting(&self) -> bool {
        self.waiting
    }
}

/// Ordered presentation outcomes from one Direct Dispatch batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchResult {
    runtime: AgentRuntime,
    outcomes: Vec<DispatchOutcome>,
    partial: bool,
    /// A spend-cap warning attached when Dispatch proceeded over the cap.
    warning: Option<String>,
}

impl DispatchResult {
    /// Creates ordered presentation outcomes from one Dispatch attempt.
    #[cfg(test)]
    pub fn new(runtime: AgentRuntime, outcomes: Vec<DispatchOutcome>, partial: bool) -> Self {
        Self {
            runtime,
            outcomes,
            partial,
            warning: None,
        }
    }

    /// Returns the Agent Runtime selected for every outcome in this batch.
    pub const fn runtime(&self) -> AgentRuntime {
        self.runtime
    }

    /// Returns outcomes in the same order as the requested Tickets.
    pub fn outcomes(&self) -> &[DispatchOutcome] {
        &self.outcomes
    }
    /// Reports whether the shared coordinator dispatched only a subset.
    pub const fn is_partial(&self) -> bool {
        self.partial
    }
    /// Returns the spend-cap warning Dispatch proceeded under, when it did.
    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    /// Attaches a spend-cap warning, for tests.
    #[cfg(test)]
    pub fn with_warning(mut self, warning: impl Into<String>) -> Self {
        self.warning = Some(warning.into());
        self
    }
}

/// Structured Dispatch failure information retained for capacity confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DispatchCapacityShortage {
    requested: usize,
    available: usize,
}

impl DispatchCapacityShortage {
    /// Creates a shortage from counts observed under the Worker Pool lock.
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
    /// Returns the idle Worker count observed under the Worker Pool lock.
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
        /// The background process identifier, when the Run drove a process.
        pid: Option<u32>,
        /// The opencode session the prompt was delivered to, when the Run drove
        /// no client process.
        session_id: Option<String>,
    },
    /// No free Worker: the Parent is waiting for capacity rather than failing.
    Waiting { parent: String, detail: String },
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
        snapshot: Box<WorkerPoolSnapshot>,
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
    /// Orchestration preparation or execution started.
    OrchestrationStarted {
        operation: OperationId,
        parent: String,
        resumed: bool,
    },
    /// A durable orchestration update was projected for the Worker Pool view.
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
        /// The background process identifier, when the Run drove a process.
        pid: Option<u32>,
        /// The opencode session the prompt was delivered to, when the Run drove
        /// no client process.
        session_id: Option<String>,
    },
    /// Orchestration found no free Worker, so the Parent waits for capacity.
    OrchestrationWaiting {
        operation: OperationId,
        parent: String,
        detail: String,
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
                    snapshot: Box::new(snapshot),
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
                            snapshot: Box::new(snapshot),
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
                            snapshot: Box::new(snapshot),
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
            WorkspaceDispatchCommand::DiscoverReady { operation } => {
                match adapter.discover_ready() {
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
                        session_id,
                    } => {
                        emit(WorkspaceDispatchEvent::OrchestrationDirect {
                            operation,
                            parent,
                            worker,
                            pid,
                            session_id,
                        });
                    }
                    WorkspaceDispatchOrchestrationEvent::Terminal { parent, counts } => {
                        emit(WorkspaceDispatchEvent::OrchestrationTerminal {
                            operation,
                            parent,
                            counts,
                        });
                    }
                    WorkspaceDispatchOrchestrationEvent::Waiting { parent, detail } => {
                        emit(WorkspaceDispatchEvent::OrchestrationWaiting {
                            operation,
                            parent,
                            detail,
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
            } => match adapter.send(worker.as_deref(), &prompt) {
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
                            snapshot: Box::new(snapshot),
                        }),
                        Err(message) => emit(WorkspaceDispatchEvent::Failed { operation, message }),
                    }
                    let restoration_emit = Arc::clone(&emit);
                    std::thread::spawn(move || {
                        let result = match restoration {
                            crate::engine::WorkspaceRestoration::SkippedMissingWorkspace => {
                                WorkspaceRestorationResult::Skipped
                            }
                            crate::engine::WorkspaceRestoration::Pending(handle) => {
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
            WorkspaceDispatchCommand::ResolvePermission {
                operation,
                worker,
                decision,
            } => match adapter.resolve_permission(&worker, decision) {
                Ok(result) => {
                    emit(WorkspaceDispatchEvent::WorkerCommandCompleted { operation, result })
                }
                Err(message) => emit(WorkspaceDispatchEvent::Failed { operation, message }),
            },
            WorkspaceDispatchCommand::Undo { operation, worker } => match adapter.undo(&worker) {
                Ok(result) => {
                    emit(WorkspaceDispatchEvent::WorkerCommandCompleted { operation, result })
                }
                Err(message) => emit(WorkspaceDispatchEvent::Failed { operation, message }),
            },
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

    fn result(&self, tickets: &[String], worker: Option<&str>) -> DispatchAdapterResult {
        let runtime = self
            .snapshot
            .pool()
            .and_then(|pool| pool.agent_runtime())
            .unwrap_or(AgentRuntime::Claude);
        DispatchAdapterResult::new(DispatchResult {
            runtime,
            outcomes: tickets
                .iter()
                .map(|ticket| DispatchOutcome {
                    ticket: ticket.clone(),
                    title: ticket.clone(),
                    worker: worker.map(str::to_owned),
                    pid: Some(42),
                    session_id: None,
                    phase: None,
                    detail: None,
                    waiting: false,
                })
                .collect(),
            partial: false,
            warning: None,
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
        Ok(self.result(tickets, worker))
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
        Ok(self.result(tickets, worker))
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
        Ok(self.result(tickets, worker))
    }

    fn discover_ready(&self) -> Result<ReadyTicketResult, String> {
        self.commands
            .lock()
            .expect("recording adapter lock")
            .push(WorkspaceDispatchCommand::DiscoverReady { operation: 0 });
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
        let mut state = crate::engine::DispatchGroupState::new(
            crate::engine::TicketId::parse(parent.to_owned()).map_err(|error| error.to_string())?,
            crate::engine::WireTimestamp::new("2026-08-10T10:00:00Z"),
            "Jarvvski/jjfx",
            crate::engine::DispatchGroupOptions::new(""),
        );
        state.sub_issues = std::collections::BTreeMap::new();
        let progress = crate::engine::DispatchGroup::from_state(state)
            .map_err(|error| error.to_string())?
            .progress()
            .map_err(|error| error.to_string())?;
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

    fn send(&self, worker: Option<&str>, prompt: &str) -> Result<WorkerSessionOutcome, String> {
        self.commands.lock().expect("recording adapter lock").push(
            WorkspaceDispatchCommand::Send {
                operation: 0,
                worker: worker.map(str::to_owned),
                prompt: prompt.to_owned(),
            },
        );
        Ok(WorkerSessionOutcome::new(
            worker.unwrap_or("worker-01"),
            WorkerActionKind::Send,
            AgentRuntime::Claude,
            AgentSessionResolution::Fresh {
                reason: crate::engine::FreshSessionReason::NoPriorLog,
            },
            Some(42),
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
            Some(43),
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
            WorkerResetOutcome::new(worker, crate::engine::RunReset::AlreadyIdle),
            crate::engine::WorkspaceRestoration::SkippedMissingWorkspace,
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

    fn resolve_permission(
        &self,
        worker: &str,
        decision: PermissionDecision,
    ) -> Result<WorkerCommandResult, String> {
        self.commands.lock().expect("recording adapter lock").push(
            WorkspaceDispatchCommand::ResolvePermission {
                operation: 0,
                worker: worker.to_owned(),
                decision,
            },
        );
        Ok(WorkerCommandResult::PermissionResolved {
            worker: worker.to_owned(),
            action: "bash".to_owned(),
            decision,
        })
    }

    fn undo(&self, worker: &str) -> Result<WorkerCommandResult, String> {
        self.commands.lock().expect("recording adapter lock").push(
            WorkspaceDispatchCommand::Undo {
                operation: 0,
                worker: worker.to_owned(),
            },
        );
        Ok(WorkerCommandResult::MutationUndone {
            worker: worker.to_owned(),
            kind: MutationKind::Forge,
            discarded: 2,
            later: 0,
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
    use std::process::Command;
    use tempfile::TempDir;

    fn empty_snapshot() -> WorkerPoolSnapshot {
        let directory = TempDir::new().expect("temporary repository");
        let output = Command::new("jj")
            .args([
                "--config",
                "signing.behavior=drop",
                "git",
                "init",
                "--no-colocate",
            ])
            .arg(directory.path())
            .output()
            .expect("jj should be installed");
        assert!(output.status.success());
        crate::engine::Repository::open(directory.path())
            .expect("repository should open")
            .read_worker_pool_snapshot()
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

        controller.submit(WorkspaceDispatchCommand::DiscoverReady { operation: 11 });

        assert!(matches!(
            events_rx.recv().expect("discovery event"),
            WorkspaceDispatchEvent::ReadyTickets { operation: 11, result }
                if result.tickets()[0].id() == "ENG-42"
                    && result.tickets()[0].title() == "Example ready Ticket"
        ));
        assert!(adapter.commands().iter().any(|command| matches!(
            command,
            WorkspaceDispatchCommand::DiscoverReady { operation: 0 }
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
            } if progress.parent().as_str() == "ENG-100" && notice == "orchestration test update"
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
            worker: Some("worker-01".to_owned()),
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
                            reason: crate::engine::FreshSessionReason::NoPriorLog
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
                if worker.as_deref() == Some("worker-01") && prompt == "continue the work"
        )));
        assert!(commands.iter().any(|command| matches!(
            command,
            WorkspaceDispatchCommand::Review { worker, .. } if worker == "worker-01"
        )));
    }

    #[test]
    fn controller_forwards_an_any_worker_send_target() {
        let adapter = RecordingAdapter::new(empty_snapshot());
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let controller = WorkspaceDispatchController::new(adapter.clone(), move |event| {
            events_tx.send(event).expect("event receiver");
        });

        controller.submit(WorkspaceDispatchCommand::Send {
            operation: 42,
            worker: None,
            prompt: "take the next task".to_owned(),
        });

        assert!(matches!(
            events_rx.recv().expect("send event"),
            WorkspaceDispatchEvent::WorkerActionCompleted { operation: 42, outcome }
                if outcome.worker() == "worker-01"
        ));
        assert!(adapter.commands().iter().any(|command| matches!(
            command,
            WorkspaceDispatchCommand::Send { worker, prompt, .. }
                if worker.is_none() && prompt == "take the next task"
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
                    && outcome.run() == crate::engine::RunReset::AlreadyIdle
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
        let activity = RunActivity::new(crate::engine::RunActivityKind::Message {
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
                        crate::engine::RunActivityKind::Message { text } if text == "hello from Claude"
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
}
