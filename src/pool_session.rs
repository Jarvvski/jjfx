//! The Worker Pool interaction state for the App.
//!
//! [`PoolSession::apply`] is the one place the operation-identity rules live: it
//! folds a Dispatch event, ignores it when its operation has been superseded by a
//! newer command, updates the Pool presentation, and returns the App-facing
//! [`PoolUpdate`]s that follow. The App keeps one field of this type, forwards
//! events through `apply`, and reads the projection when rendering the Pool.

use crate::workspace_dispatch::{
    DispatchCapacityShortage, DispatchGroupProgress, DispatchResult, OperationId,
    ReadyTicketResult, WorkerCommandResult, WorkerLogSnapshot, WorkerSessionOutcome,
    WorkspaceDispatchEvent, WorkspaceRestorationResult,
};
use wsg_core::WorkerPoolSnapshot;

/// The App-facing consequence of one accepted Dispatch event.
#[derive(Debug)]
pub(crate) enum PoolUpdate {
    /// Show a transient footer status.
    Status(String),
    /// Ask the controller for a fresh Pool snapshot.
    RefreshPool,
    /// Ask the operator to confirm growing the Pool for these Tickets.
    ConfirmCapacity {
        tickets: Vec<String>,
        worker: Option<String>,
        shortage: DispatchCapacityShortage,
    },
    /// Show the discovered Ready Tickets for review.
    ReadyPreview,
    /// Leave the Pool mode.
    ClosePoolMode,
}

/// Everything the App knows about the Worker Pool, owned in one place.
#[derive(Default)]
pub(crate) struct PoolSession {
    /// Latest immutable Worker Pool presentation snapshot.
    pub(crate) worker_pool: Option<WorkerPoolSnapshot>,
    /// Operation identity of the newest Pool mutation; results for older
    /// operations are ignored.
    pub(crate) active_operation: Option<OperationId>,
    /// A Reset keeps its operation active until Workspace restoration completes.
    pub(crate) worker_reset_operation: Option<OperationId>,
    /// Independent operation identity for durable Parent orchestration.
    pub(crate) orchestration_operation: Option<OperationId>,
    /// Independent operation identity for the selected Worker log watcher.
    pub(crate) worker_log_operation: Option<OperationId>,
    /// Latest immutable provider-neutral Worker log snapshot.
    pub(crate) worker_log: Option<WorkerLogSnapshot>,
    /// The latest selected-Worker Follow-up Session outcome.
    pub(crate) worker_session: Option<WorkerSessionOutcome>,
    /// The latest typed non-Run Worker action result.
    pub(crate) worker_command_result: Option<WorkerCommandResult>,
    /// Latest Worker log failure retained for the focused detail view.
    pub(crate) worker_log_error: Option<String>,
    /// Latest ordered Direct Dispatch outcomes for the Pool view.
    pub(crate) dispatch_result: Option<DispatchResult>,
    /// Latest Ready Ticket discovery result awaiting preview or launch.
    pub(crate) ready_tickets: Option<ReadyTicketResult>,
    /// Latest immutable Dispatch Group presentation projection.
    pub(crate) group_progress: Option<DispatchGroupProgress>,
}

impl PoolSession {
    /// Begin a Pool mutation that owns the active operation until it completes.
    pub(crate) fn begin_dispatch(&mut self, operation: OperationId) {
        self.active_operation = Some(operation);
    }

    /// Begin a Reset, which owns the active operation until Workspace
    /// restoration completes - not merely until the Run stops.
    pub(crate) fn begin_reset(&mut self, operation: OperationId) {
        self.active_operation = Some(operation);
        self.worker_reset_operation = Some(operation);
    }

    /// Begin a Parent orchestration with its own independent operation.
    pub(crate) fn begin_orchestration(&mut self, operation: OperationId) {
        self.orchestration_operation = Some(operation);
    }

    /// Begin watching a Worker log, clearing the previous log and failure.
    pub(crate) fn begin_log(&mut self, operation: OperationId) {
        self.worker_log_operation = Some(operation);
        self.worker_log = None;
        self.worker_log_error = None;
    }

    /// Stop watching a Worker log and return the operation to cancel.
    pub(crate) fn stop_log(&mut self) -> Option<OperationId> {
        self.worker_log_operation.take()
    }

    /// Abandon the active operation (the operator cancelled its prompt).
    pub(crate) fn cancel_dispatch(&mut self) {
        self.active_operation = None;
    }

    /// Fold one Dispatch event through the operation-identity rules.
    pub(crate) fn apply(&mut self, event: WorkspaceDispatchEvent) -> Vec<PoolUpdate> {
        match event {
            WorkspaceDispatchEvent::Snapshot {
                operation,
                snapshot,
            } => self.apply_snapshot(operation, snapshot),
            WorkspaceDispatchEvent::Resized { operation, result } => {
                if self.active_operation == Some(operation) {
                    return vec![PoolUpdate::Status(format!(
                        "Pool resized to {} ({} added, {} removed)",
                        result.capacity(),
                        result.added_workers().len(),
                        result.removed_workers().len()
                    ))];
                }
                Vec::new()
            }
            WorkspaceDispatchEvent::Destroyed { operation } => {
                if self.active_operation == Some(operation) {
                    return vec![
                        PoolUpdate::Status("Worker Pool destroyed".to_owned()),
                        PoolUpdate::ClosePoolMode,
                    ];
                }
                Vec::new()
            }
            WorkspaceDispatchEvent::Dispatched { operation, result } => {
                if self.active_operation == Some(operation) {
                    let count = result.outcomes().len();
                    let partial = result.is_partial();
                    self.dispatch_result = Some(result);
                    self.active_operation = None;
                    let status = if partial {
                        format!("Dispatched {count} Ticket(s) with partial capacity")
                    } else {
                        format!("Dispatched {count} Ticket(s)")
                    };
                    return vec![PoolUpdate::RefreshPool, PoolUpdate::Status(status)];
                }
                Vec::new()
            }
            WorkspaceDispatchEvent::DispatchCapacity {
                operation,
                tickets,
                worker,
                shortage,
            } => {
                if self.active_operation == Some(operation) {
                    return vec![
                        PoolUpdate::ConfirmCapacity {
                            tickets,
                            worker,
                            shortage,
                        },
                        PoolUpdate::Status(format!(
                            "Dispatch needs {} idle Worker(s); only {} available",
                            shortage.requested(),
                            shortage.available()
                        )),
                    ];
                }
                Vec::new()
            }
            WorkspaceDispatchEvent::ReadyTickets { operation, result } => {
                if self.active_operation == Some(operation) {
                    let count = result.tickets().len();
                    self.ready_tickets = Some(result);
                    self.active_operation = None;
                    return vec![
                        PoolUpdate::ReadyPreview,
                        PoolUpdate::Status(format!("Found {count} Ready Ticket(s)")),
                    ];
                }
                Vec::new()
            }
            WorkspaceDispatchEvent::GroupProgress {
                operation,
                progress,
            } => {
                if operation == 0
                    || self.active_operation == Some(operation)
                    || self.orchestration_operation == Some(operation)
                {
                    self.group_progress = Some(progress);
                }
                Vec::new()
            }
            WorkspaceDispatchEvent::OrchestrationStarted {
                operation,
                parent,
                resumed,
            } => {
                if self.orchestration_operation == Some(operation) {
                    return vec![PoolUpdate::Status(format!(
                        "{} Dispatch Group {parent}",
                        if resumed { "Resuming" } else { "Starting" }
                    ))];
                }
                Vec::new()
            }
            WorkspaceDispatchEvent::OrchestrationProgress {
                operation,
                progress,
                notice,
            } => {
                if self.orchestration_operation == Some(operation) {
                    self.group_progress = Some(progress);
                    if let Some(notice) = notice {
                        return vec![PoolUpdate::Status(notice)];
                    }
                }
                Vec::new()
            }
            WorkspaceDispatchEvent::OrchestrationDirect {
                operation,
                parent,
                worker,
                pid,
            } => {
                if self.orchestration_operation == Some(operation) {
                    self.orchestration_operation = None;
                    return vec![PoolUpdate::Status(format!(
                        "Dispatched Parent {parent} to {worker} (PID {pid})"
                    ))];
                }
                Vec::new()
            }
            WorkspaceDispatchEvent::OrchestrationTerminal {
                operation,
                parent,
                counts,
            } => {
                if self.orchestration_operation == Some(operation) {
                    self.orchestration_operation = None;
                    return vec![PoolUpdate::Status(format!(
                        "Dispatch Group {parent} complete: {} done, {} failed, {} skipped",
                        counts.done(),
                        counts.failed(),
                        counts.skipped()
                    ))];
                }
                Vec::new()
            }
            WorkspaceDispatchEvent::WorkerActionCompleted { operation, outcome } => {
                if self.active_operation == Some(operation) {
                    let worker = outcome.worker().to_owned();
                    let action = outcome.action();
                    let pid = outcome.pid();
                    self.worker_session = Some(outcome);
                    self.active_operation = None;
                    return vec![
                        PoolUpdate::RefreshPool,
                        PoolUpdate::Status(format!("{} {} (PID {pid})", action.as_str(), worker)),
                    ];
                }
                Vec::new()
            }
            WorkspaceDispatchEvent::WorkerResetCompleted { operation, outcome } => {
                if self.active_operation == Some(operation) {
                    return vec![
                        PoolUpdate::Status(format!(
                            "Reset {} ({:?}) to idle",
                            outcome.worker(),
                            outcome.run()
                        )),
                        PoolUpdate::RefreshPool,
                    ];
                }
                Vec::new()
            }
            WorkspaceDispatchEvent::WorkerCommandCompleted { operation, result } => {
                if self.active_operation == Some(operation) {
                    self.worker_command_result = Some(result.clone());
                    self.active_operation = None;
                    return vec![PoolUpdate::RefreshPool, PoolUpdate::Status(result.notice())];
                }
                Vec::new()
            }
            WorkspaceDispatchEvent::WorkspaceRestorationCompleted {
                operation,
                worker,
                result,
            } => {
                if self.worker_reset_operation == Some(operation) {
                    self.worker_reset_operation = None;
                    self.active_operation = None;
                    return vec![
                        PoolUpdate::Status(match result {
                            WorkspaceRestorationResult::Skipped => {
                                format!("Reset {worker}: Workspace restoration skipped")
                            }
                            WorkspaceRestorationResult::Restored => {
                                format!("Reset {worker}: Workspace restored")
                            }
                            WorkspaceRestorationResult::Failed(error) => {
                                format!("Reset {worker}: Workspace restoration failed: {error}")
                            }
                        }),
                        PoolUpdate::RefreshPool,
                    ];
                }
                Vec::new()
            }
            WorkspaceDispatchEvent::WorkerLogUpdated {
                operation,
                snapshot,
            } => {
                if self.worker_log_operation == Some(operation) {
                    let terminal = snapshot.result().is_some();
                    self.worker_log = Some(*snapshot);
                    self.worker_log_error = None;
                    if terminal {
                        self.worker_log_operation = None;
                    }
                }
                Vec::new()
            }
            WorkspaceDispatchEvent::WorkerLogFailed {
                operation,
                worker: _,
                message,
            } => {
                if self.worker_log_operation == Some(operation) {
                    self.worker_log_operation = None;
                    self.worker_log_error = Some(message);
                }
                Vec::new()
            }
            WorkspaceDispatchEvent::Failed { operation, message } => {
                if self.active_operation == Some(operation) {
                    self.active_operation = None;
                    self.worker_reset_operation = None;
                    return vec![PoolUpdate::Status(format!("Worker Pool: {message}"))];
                }
                if self.orchestration_operation == Some(operation) {
                    self.orchestration_operation = None;
                    return vec![PoolUpdate::Status(format!("Dispatch Group: {message}"))];
                }
                Vec::new()
            }
        }
    }

    /// Fold a Pool snapshot, honouring the active-operation rules.
    fn apply_snapshot(
        &mut self,
        operation: OperationId,
        snapshot: WorkerPoolSnapshot,
    ) -> Vec<PoolUpdate> {
        if operation == 0 {
            if self.active_operation.is_some() {
                return Vec::new();
            }
        } else if self.active_operation != Some(operation) {
            return Vec::new();
        }
        if self.worker_pool.as_ref() == Some(&snapshot) {
            if operation != 0 {
                self.active_operation = None;
            }
            return Vec::new();
        }
        self.worker_pool = Some(snapshot);
        if operation != 0 && self.worker_reset_operation != Some(operation) {
            self.active_operation = None;
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_dispatch::{DispatchOutcome, ReadyTicket, WorkerActionKind};
    use wsg_core::AgentRuntime;

    fn dispatch_result(partial: bool) -> DispatchResult {
        DispatchResult::new(
            AgentRuntime::Claude,
            vec![DispatchOutcome::success(
                "ENG-1".to_owned(),
                "First ticket".to_owned(),
                "worker-01".to_owned(),
                4242,
            )],
            partial,
        )
    }

    fn log(terminal: bool) -> WorkerLogSnapshot {
        WorkerLogSnapshot::new(
            "worker-01",
            AgentRuntime::Claude,
            None,
            terminal.then(wsg_core::RunResult::succeeded),
        )
    }

    #[test]
    fn newer_dispatch_supersedes_older_results() {
        let mut pool = PoolSession::default();
        pool.begin_dispatch(4);

        let stale = pool.apply(WorkspaceDispatchEvent::Dispatched {
            operation: 3,
            result: dispatch_result(false),
        });
        assert!(stale.is_empty());
        assert!(pool.dispatch_result.is_none());
        assert_eq!(pool.active_operation, Some(4));

        let accepted = pool.apply(WorkspaceDispatchEvent::Dispatched {
            operation: 4,
            result: dispatch_result(false),
        });
        assert_eq!(pool.active_operation, None);
        assert_eq!(pool.dispatch_result.as_ref().unwrap().outcomes().len(), 1);
        assert!(
            matches!(accepted.as_slice(), [PoolUpdate::RefreshPool, PoolUpdate::Status(message)] if message == "Dispatched 1 Ticket(s)")
        );
    }

    #[test]
    fn reset_keeps_the_active_operation_until_workspace_restoration() {
        let mut pool = PoolSession::default();
        pool.begin_reset(8);

        let acknowledged = pool.apply(WorkspaceDispatchEvent::WorkerResetCompleted {
            operation: 8,
            outcome: crate::workspace_dispatch::WorkerResetOutcome::new(
                "worker-01",
                wsg_core::RunReset::AlreadyIdle,
            ),
        });
        assert!(
            acknowledged
                .iter()
                .any(|u| matches!(u, PoolUpdate::RefreshPool))
        );
        assert_eq!(pool.active_operation, Some(8));
        assert_eq!(pool.worker_reset_operation, Some(8));

        let restored = pool.apply(WorkspaceDispatchEvent::WorkspaceRestorationCompleted {
            operation: 8,
            worker: "worker-01".to_owned(),
            result: WorkspaceRestorationResult::Restored,
        });
        assert_eq!(pool.active_operation, None);
        assert_eq!(pool.worker_reset_operation, None);
        assert!(
            matches!(restored.as_slice(), [PoolUpdate::Status(message), PoolUpdate::RefreshPool] if message == "Reset worker-01: Workspace restored")
        );
    }

    #[test]
    fn a_terminal_log_snapshot_stops_the_watcher() {
        let mut pool = PoolSession::default();
        pool.begin_log(7);

        pool.apply(WorkspaceDispatchEvent::WorkerLogUpdated {
            operation: 7,
            snapshot: Box::new(log(false)),
        });
        assert_eq!(pool.worker_log_operation, Some(7));
        assert!(pool.worker_log.is_some());

        pool.apply(WorkspaceDispatchEvent::WorkerLogUpdated {
            operation: 7,
            snapshot: Box::new(log(true)),
        });
        assert_eq!(pool.worker_log_operation, None);
        assert!(pool.worker_log.as_ref().unwrap().result().is_some());
    }

    #[test]
    fn a_stale_log_event_is_ignored() {
        let mut pool = PoolSession::default();
        pool.begin_log(7);

        pool.apply(WorkspaceDispatchEvent::WorkerLogUpdated {
            operation: 6,
            snapshot: Box::new(log(false)),
        });

        assert_eq!(pool.worker_log_operation, Some(7));
        assert!(pool.worker_log.is_none());
    }

    #[test]
    fn a_log_failure_stops_the_watcher_and_records_the_reason() {
        let mut pool = PoolSession::default();
        pool.begin_log(7);

        pool.apply(WorkspaceDispatchEvent::WorkerLogFailed {
            operation: 7,
            worker: "worker-01".to_owned(),
            message: "log file unavailable".to_owned(),
        });

        assert_eq!(pool.worker_log_operation, None);
        assert_eq!(
            pool.worker_log_error.as_deref(),
            Some("log file unavailable")
        );
    }

    #[test]
    fn ready_tickets_end_the_operation_and_request_a_preview() {
        let mut pool = PoolSession::default();
        pool.begin_dispatch(9);

        let updates = pool.apply(WorkspaceDispatchEvent::ReadyTickets {
            operation: 9,
            result: ReadyTicketResult::new(
                vec![ReadyTicket::new("ENG-1", "First ticket")],
                Vec::new(),
            ),
        });

        assert_eq!(pool.active_operation, None);
        assert_eq!(pool.ready_tickets.as_ref().unwrap().tickets().len(), 1);
        assert!(
            matches!(updates.as_slice(), [PoolUpdate::ReadyPreview, PoolUpdate::Status(message)] if message == "Found 1 Ready Ticket(s)")
        );
    }

    #[test]
    fn a_capacity_shortage_keeps_the_operation_for_the_prompt() {
        let mut pool = PoolSession::default();
        pool.begin_dispatch(12);

        let updates = pool.apply(WorkspaceDispatchEvent::DispatchCapacity {
            operation: 12,
            tickets: vec!["ENG-1".to_owned()],
            worker: None,
            shortage: DispatchCapacityShortage::new(3, 1),
        });

        assert_eq!(pool.active_operation, Some(12));
        assert!(matches!(
            updates.as_slice(),
            [PoolUpdate::ConfirmCapacity { shortage, .. }, PoolUpdate::Status(_)]
                if shortage.gap() == 2
        ));
    }

    #[test]
    fn failed_clears_the_matching_operation_only() {
        let mut pool = PoolSession::default();
        pool.begin_reset(5);

        assert!(
            pool.apply(WorkspaceDispatchEvent::Failed {
                operation: 6,
                message: "other".to_owned(),
            })
            .is_empty()
        );
        assert_eq!(pool.active_operation, Some(5));

        let updates = pool.apply(WorkspaceDispatchEvent::Failed {
            operation: 5,
            message: "pool lock held".to_owned(),
        });
        assert_eq!(pool.active_operation, None);
        assert_eq!(pool.worker_reset_operation, None);
        assert!(
            matches!(updates.as_slice(), [PoolUpdate::Status(message)] if message == "Worker Pool: pool lock held")
        );
    }

    #[test]
    fn orchestration_direct_completion_clears_its_operation() {
        let mut pool = PoolSession::default();
        pool.begin_orchestration(2);

        let updates = pool.apply(WorkspaceDispatchEvent::OrchestrationDirect {
            operation: 2,
            parent: "ENG-100".to_owned(),
            worker: "worker-02".to_owned(),
            pid: 99,
        });

        assert_eq!(pool.orchestration_operation, None);
        assert!(
            matches!(updates.as_slice(), [PoolUpdate::Status(message)] if message == "Dispatched Parent ENG-100 to worker-02 (PID 99)")
        );
    }

    #[test]
    fn cancel_dispatch_abandons_the_active_operation() {
        let mut pool = PoolSession::default();
        pool.begin_dispatch(3);
        pool.cancel_dispatch();
        assert_eq!(pool.active_operation, None);
    }

    #[test]
    fn worker_action_outcome_reports_its_pid() {
        let mut pool = PoolSession::default();
        pool.begin_dispatch(1);

        let updates = pool.apply(WorkspaceDispatchEvent::WorkerActionCompleted {
            operation: 1,
            outcome: WorkerSessionOutcome::new(
                "worker-01",
                WorkerActionKind::Send,
                AgentRuntime::Claude,
                wsg_core::AgentSessionResolution::Fresh {
                    reason: wsg_core::FreshSessionReason::MissingIdentity,
                },
                777,
            ),
        });

        assert!(pool.worker_session.is_some());
        assert!(
            matches!(updates.as_slice(), [PoolUpdate::RefreshPool, PoolUpdate::Status(message)] if message == "Send worker-01 (PID 777)")
        );
    }
}
