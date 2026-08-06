//! The Workspace Dispatch command/event seam used by the jjfx App.
//!
//! The App submits a small command vocabulary and receives immutable events.
//! Adapters own repository access, locks, blocking work, and error mapping.

use std::path::PathBuf;
use std::sync::Arc;

use wsg_core::{
    AgentRuntime, AgentRuntimeQuery, DirectDispatchError, DirectDispatchExecution,
    DirectDispatchFailurePhase, DirectDispatchOutcome, DirectDispatchRequest, PoolCapacity,
    ReadyTicketFilter, RunMode, TicketDiscovery, TicketId, TicketStatus, WorkerId, WorkerPoolError,
    WorkerPoolSnapshot,
};

/// A user-visible operation identity used to ignore stale results.
pub type OperationId = u64;

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
    fn success(ticket: String, title: String, worker: String, pid: u32) -> Self {
        Self {
            ticket,
            title,
            worker: Some(worker),
            pid: Some(pid),
            phase: None,
            detail: None,
        }
    }

    fn failure(
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
}

/// A deep asynchronous controller for Workspace Dispatch operations.
#[derive(Clone)]
pub struct WorkspaceDispatchController {
    adapter: Arc<dyn WorkspaceDispatchAdapter>,
    emit: Arc<dyn Fn(WorkspaceDispatchEvent) + Send + Sync>,
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
        }
    }

    /// Submits a blocking command and returns immediately.
    pub fn submit(&self, command: WorkspaceDispatchCommand) {
        let adapter = Arc::clone(&self.adapter);
        let emit = Arc::clone(&self.emit);
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
        });
    }
}

/// The production adapter over the shared Repository-owned Pool module.
#[derive(Debug, Clone)]
pub struct RealWorkspaceDispatch {
    repository_root: PathBuf,
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
        let discovery = TicketDiscovery::new(AgentRuntimeQuery::new(runtime, repository.root()));
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
}

/// Test adapter that records commands while returning a supplied snapshot.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct RecordingAdapter {
    commands: Arc<std::sync::Mutex<Vec<WorkspaceDispatchCommand>>>,
    snapshot: WorkerPoolSnapshot,
}

#[cfg(test)]
impl RecordingAdapter {
    pub(crate) fn new(snapshot: WorkerPoolSnapshot) -> Self {
        Self {
            commands: Arc::new(std::sync::Mutex::new(Vec::new())),
            snapshot,
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

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
