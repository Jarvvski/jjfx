//! Persistent, frontend-neutral Dispatch Group orchestration.
//!
//! Frontends select foreground or detached execution and render typed events.
//! This module owns orchestration order while keeping Worker Pool, Direct
//! Dispatch, compatible persistence, and terminal formatting behind one seam.

use std::path::Path;

use thiserror::Error;

use crate::{
    AgentRuntime, DispatchGroupError, DispatchGroupStatusCounts, Repository, TicketId, WorkerId,
};

#[cfg(test)]
use crate::{DispatchGroup, DispatchGroupEvent, SubIssueStatus, WireTimestamp, WorkerStatus};
#[cfg(test)]
use std::collections::BTreeMap;

/// Inputs required to start or resume one Parent Ticket's orchestration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestrationRequest {
    parent: TicketId,
    agent_runtime: AgentRuntime,
    model: Option<String>,
}

impl OrchestrationRequest {
    /// Creates a request using provider-managed model selection.
    pub fn new(parent: TicketId, agent_runtime: AgentRuntime) -> Self {
        Self {
            parent,
            agent_runtime,
            model: None,
        }
    }

    /// Supplies a caller-selected model override.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        let model = model.into();
        self.model = (!model.trim().is_empty()).then_some(model);
        self
    }

    /// Returns the Parent Ticket being orchestrated.
    pub fn parent(&self) -> &TicketId {
        &self.parent
    }

    /// Returns the Agent Runtime used for dependency discovery.
    pub const fn agent_runtime(&self) -> AgentRuntime {
        self.agent_runtime
    }

    /// Returns the optional model override persisted with a new group.
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }
}

/// A formatting-free orchestration progress notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrchestrationEvent {
    /// The runner acquired ownership and loaded or created the group.
    Started {
        /// Parent Ticket being watched.
        parent: TicketId,
        /// Whether compatible state already existed.
        resumed: bool,
    },
    /// One completed Run was durably folded into the group.
    Completed {
        /// Completed Sub-issue.
        ticket: TicketId,
        /// Worker that produced the result.
        worker: WorkerId,
        /// Resulting bookmark, when one was discovered.
        branch: Option<String>,
    },
    /// A failed first attempt was Reset and made dispatchable again.
    Retrying {
        /// Sub-issue being retried.
        ticket: TicketId,
        /// Reset Worker.
        worker: WorkerId,
        /// One-based next attempt number.
        attempt: u8,
    },
    /// One ready Sub-issue was durably assigned and launched.
    Dispatched {
        /// Launched Sub-issue.
        ticket: TicketId,
        /// Assigned Worker.
        worker: WorkerId,
    },
    /// A ready Sub-issue is waiting for reusable Worker capacity.
    WaitingForCapacity {
        /// Sub-issue left pending.
        ticket: TicketId,
    },
    /// A failed launch was compensated without consuming a retry.
    LaunchFailed {
        /// Sub-issue returned to pending.
        ticket: TicketId,
        /// Worker whose Reservation was released.
        worker: WorkerId,
        /// Context from the failed launch.
        detail: String,
    },
    /// A persisted dependency bookmark was repaired before resume.
    BranchRevalidated {
        /// Sub-issue whose result bookmark changed.
        ticket: TicketId,
        /// Persisted bookmark that no longer resolved.
        previous: String,
        /// Replacement bookmark or `main` fallback.
        current: String,
    },
    /// A repeated failed Run reached terminal failure.
    Failed {
        /// Terminally failed Sub-issue.
        ticket: TicketId,
        /// Worker that ran the final attempt.
        worker: WorkerId,
        /// Persisted Worker failure context, when available.
        detail: Option<String>,
    },
    /// Every Sub-issue reached a terminal state.
    Terminal(OrchestrationSummary),
}

/// Terminal information returned without choosing terminal formatting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestrationSummary {
    parent: TicketId,
    counts: DispatchGroupStatusCounts,
    direct_worker: Option<WorkerId>,
}

impl OrchestrationSummary {
    /// Returns the Parent Ticket represented by this outcome.
    pub fn parent(&self) -> &TicketId {
        &self.parent
    }

    /// Returns done, failed, and skipped Sub-issue counts.
    pub const fn counts(&self) -> DispatchGroupStatusCounts {
        self.counts
    }

    /// Returns the Worker used when a Parent with no Sub-issues fell back to Direct Dispatch.
    pub const fn direct_worker(&self) -> Option<&WorkerId> {
        self.direct_worker.as_ref()
    }
}

/// A failure to start, advance, or resume persistent orchestration.
#[derive(Debug, Error)]
pub enum OrchestrationError {
    /// Compatible Dispatch Group state violated a domain invariant.
    #[error(transparent)]
    DispatchGroup(#[from] DispatchGroupError),
    /// A dispatched Sub-issue has no persisted Worker assignment.
    #[error("dispatched Ticket {ticket} has no Worker assignment")]
    UnassignedWorker {
        /// Dispatched Sub-issue.
        ticket: TicketId,
    },
    /// A dispatched Sub-issue references a Worker that is not observable.
    #[error("dispatched Ticket {ticket} references missing Worker {worker}")]
    MissingWorker {
        /// Dispatched Sub-issue.
        ticket: TicketId,
        /// Missing Worker assignment.
        worker: WorkerId,
    },
    /// A Worker's persisted Ticket differs from its Dispatch Group assignment.
    #[error("Worker {worker} belongs to Ticket {actual:?}, not dispatched Ticket {expected}")]
    WorkerTicketMismatch {
        /// Worker whose assignment disagrees.
        worker: WorkerId,
        /// Ticket expected by the Dispatch Group.
        expected: TicketId,
        /// Ticket currently persisted on the Worker.
        actual: Option<String>,
    },
    /// Another runner committed the Dispatch Group first.
    #[error("Dispatch Group {parent} changed concurrently")]
    ConcurrentChange {
        /// Parent Ticket whose optimistic revision was stale.
        parent: TicketId,
    },
    /// A production or test execution adapter failed.
    #[error("orchestration execution failed: {0}")]
    Execution(String),
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct WorkerObservation {
    worker: WorkerId,
    status: WorkerStatus,
    ticket: Option<String>,
    branch: Option<String>,
}

#[cfg(test)]
struct LoadedOrchestration<R> {
    group: DispatchGroup,
    revision: R,
}

#[cfg(test)]
trait OrchestrationExecution {
    type Revision;

    fn load_group(
        &mut self,
        parent: &TicketId,
    ) -> Result<LoadedOrchestration<Self::Revision>, OrchestrationError>;
    fn workers(&mut self) -> Result<Vec<WorkerObservation>, OrchestrationError>;
    fn save_group(
        &mut self,
        expected: &Self::Revision,
        group: &DispatchGroup,
    ) -> Result<Self::Revision, OrchestrationError>;
    fn reset_worker(&mut self, worker: &WorkerId) -> Result<(), OrchestrationError>;
    fn claim_ready(&mut self, ticket: &TicketId) -> Result<(), OrchestrationError>;
    fn now(&mut self) -> Result<WireTimestamp, OrchestrationError>;
}

#[cfg(test)]
fn run_with_execution<E: OrchestrationExecution>(
    request: &OrchestrationRequest,
    execution: &mut E,
    observer: &mut impl FnMut(OrchestrationEvent),
) -> Result<E::Revision, OrchestrationError> {
    let loaded = execution.load_group(request.parent())?;
    let mut group = loaded.group;
    let mut revision = loaded.revision;
    let workers = execution
        .workers()?
        .into_iter()
        .map(|worker| (worker.worker.clone(), worker))
        .collect::<BTreeMap<_, _>>();
    let dispatched = group
        .state()
        .sub_issues
        .iter()
        .filter(|&(_ticket, issue)| {
            SubIssueStatus::try_from(&issue.status).ok() == Some(SubIssueStatus::Dispatched)
        })
        .map(|(ticket, issue)| (ticket.clone(), issue.worker.clone()))
        .collect::<Vec<_>>();

    for (ticket, assigned_worker) in dispatched {
        let worker = assigned_worker.ok_or_else(|| OrchestrationError::UnassignedWorker {
            ticket: ticket.clone(),
        })?;
        let observation =
            workers
                .get(&worker)
                .ok_or_else(|| OrchestrationError::MissingWorker {
                    ticket: ticket.clone(),
                    worker: worker.clone(),
                })?;
        if observation.ticket.as_deref() != Some(ticket.as_str()) {
            return Err(OrchestrationError::WorkerTicketMismatch {
                worker,
                expected: ticket,
                actual: observation.ticket.clone(),
            });
        }
        if observation.status != WorkerStatus::Done {
            continue;
        }
        let branch = observation.branch.clone();
        group.apply(DispatchGroupEvent::Completed {
            ticket: ticket.clone(),
            worker: worker.clone(),
            branch: branch.clone(),
            at: execution.now()?,
        })?;
        revision = execution.save_group(&revision, &group)?;
        observer(OrchestrationEvent::Completed {
            ticket,
            worker: worker.clone(),
            branch,
        });
        execution.reset_worker(&worker)?;
    }

    for ticket in group.ready() {
        execution.claim_ready(&ticket)?;
    }
    Ok(revision)
}

/// The deep application interface for persistent orchestration.
#[derive(Debug, Clone)]
pub struct OrchestrationRunner {
    repository: Repository,
}

impl OrchestrationRunner {
    pub(crate) fn new(repository: Repository) -> Self {
        Self { repository }
    }

    /// Returns the Repository root owned by this runner.
    pub fn repository_root(&self) -> &Path {
        self.repository.root()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DispatchGroupOptions, DispatchGroupState, SubIssueState, WireStatus};

    struct FakeExecution {
        group: DispatchGroup,
        revision: u64,
        workers: Vec<WorkerObservation>,
        calls: Vec<String>,
        fail_next_save: bool,
    }

    impl FakeExecution {
        fn completed_blocker_chain() -> Self {
            let blocker = TicketId::parse("ENG-101").expect("Blocker Ticket");
            let dependent = TicketId::parse("ENG-102").expect("dependent Ticket");
            let worker = WorkerId::parse("worker-01").expect("Worker");
            let mut state = DispatchGroupState::new(
                TicketId::parse("ENG-100").expect("Parent Ticket"),
                WireTimestamp::new("2026-08-04T12:00:00Z"),
                "owner/repo",
                DispatchGroupOptions::new(""),
            );
            let mut completed_blocker = SubIssueState::new(
                "Build the foundation",
                WireStatus::new("dispatched"),
                Vec::new(),
            );
            completed_blocker.worker = Some(worker.clone());
            completed_blocker.dispatched_at = Some(WireTimestamp::new("2026-08-04T12:01:00Z"));
            state.sub_issues.insert(blocker.clone(), completed_blocker);
            state.sub_issues.insert(
                dependent,
                SubIssueState::new(
                    "Use the foundation",
                    WireStatus::new("pending"),
                    vec![blocker.clone()],
                ),
            );
            Self {
                group: DispatchGroup::from_state(state).expect("valid group"),
                revision: 0,
                workers: vec![WorkerObservation {
                    worker,
                    status: WorkerStatus::Done,
                    ticket: Some(blocker.as_str().to_owned()),
                    branch: Some("adam/eng-101-foundation".to_owned()),
                }],
                calls: Vec::new(),
                fail_next_save: false,
            }
        }

        fn fail_next_save(&mut self) {
            self.fail_next_save = true;
        }
    }

    impl OrchestrationExecution for FakeExecution {
        type Revision = u64;

        fn load_group(
            &mut self,
            _parent: &TicketId,
        ) -> Result<LoadedOrchestration<Self::Revision>, OrchestrationError> {
            Ok(LoadedOrchestration {
                group: self.group.clone(),
                revision: self.revision,
            })
        }

        fn workers(&mut self) -> Result<Vec<WorkerObservation>, OrchestrationError> {
            self.calls.push("workers".to_owned());
            Ok(self.workers.clone())
        }

        fn save_group(
            &mut self,
            expected: &Self::Revision,
            group: &DispatchGroup,
        ) -> Result<Self::Revision, OrchestrationError> {
            let ticket = TicketId::parse("ENG-101").expect("Ticket");
            let status = group
                .state()
                .sub_issues
                .get(&ticket)
                .expect("saved Ticket")
                .status
                .as_str();
            self.calls
                .push(format!("save:r{expected}:{ticket}:{status}"));
            if self.fail_next_save || *expected != self.revision {
                self.fail_next_save = false;
                return Err(OrchestrationError::ConcurrentChange {
                    parent: group.state().parent.clone(),
                });
            }
            self.group = group.clone();
            self.revision += 1;
            Ok(self.revision)
        }

        fn reset_worker(&mut self, worker: &WorkerId) -> Result<(), OrchestrationError> {
            self.calls.push(format!("reset:{worker}"));
            Ok(())
        }

        fn claim_ready(&mut self, ticket: &TicketId) -> Result<(), OrchestrationError> {
            self.calls.push(format!("claim:{ticket}"));
            Ok(())
        }

        fn now(&mut self) -> Result<WireTimestamp, OrchestrationError> {
            Ok(WireTimestamp::new("2026-08-04T12:02:00Z"))
        }
    }

    #[test]
    fn a_persistence_conflict_stops_before_events_resets_or_new_claims() {
        let request = OrchestrationRequest::new(
            TicketId::parse("ENG-100").expect("Parent Ticket"),
            AgentRuntime::Claude,
        );
        let mut execution = FakeExecution::completed_blocker_chain();
        execution.fail_next_save();
        let mut events = Vec::new();

        let error = run_with_execution(&request, &mut execution, &mut |event| events.push(event))
            .expect_err("stale revision must stop the advance");

        assert!(matches!(error, OrchestrationError::ConcurrentChange { .. }));
        assert_eq!(
            execution.calls,
            vec!["workers".to_owned(), "save:r0:ENG-101:done".to_owned()]
        );
        assert!(events.is_empty());
    }

    #[test]
    fn existing_workers_are_reconciled_before_ready_dependents_are_claimed() {
        let request = OrchestrationRequest::new(
            TicketId::parse("ENG-100").expect("Parent Ticket"),
            AgentRuntime::Claude,
        );
        let mut execution = FakeExecution::completed_blocker_chain();
        let mut events = Vec::new();

        run_with_execution(&request, &mut execution, &mut |event| events.push(event))
            .expect("advance orchestration");

        assert_eq!(
            execution.calls,
            vec![
                "workers".to_owned(),
                "save:r0:ENG-101:done".to_owned(),
                "reset:worker-01".to_owned(),
                "claim:ENG-102".to_owned(),
            ]
        );
        assert!(matches!(
            events.first(),
            Some(OrchestrationEvent::Completed { ticket, .. }) if ticket.as_str() == "ENG-101"
        ));
    }
}
