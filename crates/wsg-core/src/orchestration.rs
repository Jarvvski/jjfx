//! Persistent, frontend-neutral Dispatch Group orchestration.
//!
//! Frontends select foreground or detached execution and render typed events.
//! This module owns orchestration order while keeping Worker Pool, Direct
//! Dispatch, compatible persistence, and terminal formatting behind one seam.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use rustix::fs::{FlockOperation, flock};

use thiserror::Error;

use crate::pool::current_timestamp;
use crate::{
    AgentRuntime, CommitOutcome, DirectDispatchError, DirectDispatchRequest, DirectDispatchSuccess,
    DispatchGroup, DispatchGroupBuildOptions, DispatchGroupError, DispatchGroupEvent,
    DispatchGroupOptions, DispatchGroupState, DispatchGroupStatusCounts, Expected, Loaded,
    ParentTicket, Repository, RepositoryIdentity, Reservation, RunMode, StateChange, StateRevision,
    SubIssueStatus, Ticket, TicketDiscovery, TicketId, TicketQuery, TicketStatus, TicketTitle,
    WireAgent, WireTimestamp, WorkerActions, WorkerId, WorkerPoolError, WorkerStatus,
    WorkspaceRestoration,
};

/// Inputs required to start or resume one Parent Ticket's orchestration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestrationRequest {
    parent: TicketId,
    agent_runtime: AgentRuntime,
    model: Option<String>,
}

/// Polling and retry limits for one foreground or detached orchestration run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestrationOptions {
    poll_interval: Duration,
    max_cycles: Option<usize>,
}

impl Default for OrchestrationOptions {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            max_cycles: None,
        }
    }
}

impl OrchestrationOptions {
    /// Creates production polling options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the testable polling interval. Production runs cap this at five seconds.
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Bounds the number of advance cycles, including the first cycle.
    pub fn with_max_cycles(mut self, cycles: usize) -> Self {
        self.max_cycles = Some(cycles);
        self
    }

    /// Returns the configured polling interval.
    pub const fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    /// Returns the optional cycle bound.
    pub const fn max_cycles(&self) -> Option<usize> {
        self.max_cycles
    }
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

/// The result of graph discovery before orchestration begins.
#[derive(Debug)]
pub enum OrchestrationStart {
    /// A persisted Dispatch Group is ready for foreground or detached watching.
    Group,
    /// No children existed, so the reserved placeholder launched the Parent directly.
    Direct(Box<DirectDispatchSuccess>),
}

/// The result of preparing a Parent Ticket for foreground or detached watching.
#[derive(Debug)]
pub struct OrchestrationPreparation {
    start: OrchestrationStart,
    resumed: bool,
    maximum_wave: usize,
}

impl OrchestrationPreparation {
    /// Returns whether a persisted Dispatch Group was resumed.
    pub const fn resumed(&self) -> bool {
        self.resumed
    }

    /// Returns the largest dependency wave that must fit in the Worker Pool.
    pub const fn maximum_wave(&self) -> usize {
        self.maximum_wave
    }

    /// Returns the prepared group or direct Parent fallback.
    pub const fn start(&self) -> &OrchestrationStart {
        &self.start
    }

    /// Consumes the preparation and returns its start outcome.
    pub fn into_start(self) -> OrchestrationStart {
        self.start
    }
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
    /// No compatible Dispatch Group exists for a one-tick advance.
    #[error("Dispatch Group {parent} does not exist")]
    MissingGroup {
        /// Requested Parent Ticket.
        parent: TicketId,
    },
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
    /// Another runner already owns this Parent Ticket.
    #[error("Dispatch Group {parent} is already running")]
    AlreadyRunning {
        /// Parent Ticket whose runner lock is held.
        parent: TicketId,
    },
    /// Discovery failed while a placeholder Worker was reserved.
    #[error(
        "discovery failed: {primary}; additionally failed to release placeholder Reservation: {cleanup}"
    )]
    DiscoveryCleanup {
        /// Original graph discovery failure.
        primary: String,
        /// Placeholder release failure.
        cleanup: String,
    },
    /// The configured polling cycle bound was reached before terminal state.
    #[error("Dispatch Group {parent} did not reach terminal state within {cycles} cycles")]
    PollingExhausted {
        /// Parent Ticket being watched.
        parent: TicketId,
        /// Number of completed advance cycles.
        cycles: usize,
    },
}

#[derive(Debug, Clone)]
struct WorkerObservation {
    worker: WorkerId,
    status: WorkerStatus,
    ticket: Option<String>,
    branch: Option<String>,
    error: Option<String>,
}

struct LoadedOrchestration<R> {
    group: DispatchGroup,
    revision: R,
}

struct LaunchFailure {
    detail: String,
    compensated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BranchRepair {
    ticket: TicketId,
    previous: String,
    current: String,
}

trait OrchestrationExecution {
    type Revision;
    type Claim;

    fn load_group(
        &mut self,
        parent: &TicketId,
    ) -> Result<LoadedOrchestration<Self::Revision>, OrchestrationError>;
    fn workers(&mut self) -> Result<Vec<WorkerObservation>, OrchestrationError>;
    fn revalidate_branches(
        &mut self,
        group: &mut DispatchGroup,
    ) -> Result<Vec<BranchRepair>, OrchestrationError>;
    fn save_group(
        &mut self,
        expected: &Self::Revision,
        group: &DispatchGroup,
    ) -> Result<Self::Revision, OrchestrationError>;
    fn reset_worker(&mut self, worker: &WorkerId) -> Result<(), OrchestrationError>;
    fn claim(
        &mut self,
        request: &DirectDispatchRequest,
    ) -> Result<Option<Self::Claim>, OrchestrationError>;
    fn claimed_worker(claim: &Self::Claim) -> &WorkerId;
    fn release(&mut self, claim: Self::Claim) -> Result<(), OrchestrationError>;
    fn launch(
        &mut self,
        claim: Self::Claim,
        request: &DirectDispatchRequest,
    ) -> Result<(), LaunchFailure>;
    fn now(&mut self) -> Result<WireTimestamp, OrchestrationError>;
}

fn run_with_execution<E: OrchestrationExecution>(
    request: &OrchestrationRequest,
    execution: &mut E,
    observer: &mut impl FnMut(OrchestrationEvent),
) -> Result<E::Revision, OrchestrationError> {
    let loaded = execution.load_group(request.parent())?;
    let mut group = loaded.group;
    let mut revision = loaded.revision;
    for repair in execution.revalidate_branches(&mut group)? {
        revision = execution.save_group(&revision, &group)?;
        observer(OrchestrationEvent::BranchRevalidated {
            ticket: repair.ticket,
            previous: repair.previous,
            current: repair.current,
        });
    }
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
        .map(|(ticket, issue)| (ticket.clone(), issue.worker.clone(), issue.retries))
        .collect::<Vec<_>>();

    for (ticket, assigned_worker, retries) in dispatched {
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
        match observation.status {
            WorkerStatus::Done => {
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
            WorkerStatus::Failed if retries < 1 => {
                execution.reset_worker(&worker)?;
                let _retry_required = group.apply(DispatchGroupEvent::Failed {
                    ticket: ticket.clone(),
                    worker: worker.clone(),
                    at: execution.now()?,
                })?;
                group.apply(DispatchGroupEvent::Retried {
                    ticket: ticket.clone(),
                    worker: worker.clone(),
                })?;
                revision = execution.save_group(&revision, &group)?;
                observer(OrchestrationEvent::Retrying {
                    ticket,
                    worker,
                    attempt: 2,
                });
            }
            WorkerStatus::Failed => {
                group.apply(DispatchGroupEvent::Failed {
                    ticket: ticket.clone(),
                    worker: worker.clone(),
                    at: execution.now()?,
                })?;
                revision = execution.save_group(&revision, &group)?;
                observer(OrchestrationEvent::Failed {
                    ticket,
                    worker,
                    detail: observation.error.clone(),
                });
            }
            WorkerStatus::Idle | WorkerStatus::Busy => {}
        }
    }

    for ticket in group.ready() {
        let dispatch = dispatch_request(&group, &ticket, request.model())?;
        let Some(claim) = execution.claim(&dispatch)? else {
            observer(OrchestrationEvent::WaitingForCapacity { ticket });
            continue;
        };
        let worker = E::claimed_worker(&claim).clone();
        group.apply(DispatchGroupEvent::Dispatched {
            ticket: ticket.clone(),
            worker: worker.clone(),
            at: execution.now()?,
        })?;
        revision = match execution.save_group(&revision, &group) {
            Ok(revision) => revision,
            Err(primary) => {
                if let Err(cleanup) = execution.release(claim) {
                    return Err(OrchestrationError::Execution(format!(
                        "{primary}; additionally failed to release Reservation: {cleanup}"
                    )));
                }
                return Err(primary);
            }
        };
        match execution.launch(claim, &dispatch) {
            Ok(()) => observer(OrchestrationEvent::Dispatched { ticket, worker }),
            Err(failure) if failure.compensated => {
                group.apply(DispatchGroupEvent::DispatchAborted {
                    ticket: ticket.clone(),
                    worker: worker.clone(),
                })?;
                revision = execution.save_group(&revision, &group)?;
                observer(OrchestrationEvent::LaunchFailed {
                    ticket,
                    worker,
                    detail: failure.detail,
                });
            }
            Err(failure) => return Err(OrchestrationError::Execution(failure.detail)),
        }
    }
    Ok(revision)
}

fn dispatch_request(
    group: &DispatchGroup,
    ticket: &TicketId,
    model: Option<&str>,
) -> Result<DirectDispatchRequest, OrchestrationError> {
    let issue =
        group.state().sub_issues.get(ticket).ok_or_else(|| {
            OrchestrationError::Execution(format!("unknown ready Ticket {ticket}"))
        })?;
    let title = TicketTitle::parse(&issue.title)
        .map_err(|error| OrchestrationError::Execution(error.to_string()))?;
    let status = TicketStatus::parse("Todo")
        .map_err(|error| OrchestrationError::Execution(error.to_string()))?;
    let mut request = DirectDispatchRequest::new(
        Ticket::new(ticket.clone(), title, status),
        RunMode::Background,
    );
    if let Some(model) = model {
        request = request.with_model(model);
    }
    if let Some(context) = group.dependency_context(ticket)? {
        request = request.with_dependency_context(context);
    }
    Ok(request)
}

fn revalidate_branches(root: &Path, group: &mut DispatchGroup) -> Vec<BranchRepair> {
    let mut repairs = Vec::new();
    let ticket_ids = group.state().sub_issues.keys().cloned().collect::<Vec<_>>();
    for ticket in ticket_ids {
        let Some(previous) = group
            .state()
            .sub_issues
            .get(&ticket)
            .and_then(|issue| issue.branch.clone())
        else {
            continue;
        };
        if previous == "main" || revision_exists(root, &previous) {
            continue;
        }
        let current = resolve_ticket_branch(root, &ticket).unwrap_or_else(|| "main".to_owned());
        if let Some(issue) = group.state_mut().sub_issues.get_mut(&ticket) {
            issue.branch = Some(current.clone());
        }
        repairs.push(BranchRepair {
            ticket,
            previous,
            current,
        });
    }
    repairs
}

fn revision_exists(root: &Path, revision: &str) -> bool {
    Command::new("jj")
        .args([
            "log",
            "-r",
            revision,
            "--no-graph",
            "-T",
            "\"ok\"",
            "--limit",
            "1",
        ])
        .current_dir(root)
        .output()
        .is_ok_and(|output| output.status.success() && output.stdout == b"ok")
}

fn resolve_ticket_branch(root: &Path, ticket: &TicketId) -> Option<String> {
    let prefix = format!("adam/{}-", ticket.as_str().to_ascii_lowercase());
    let output = Command::new("jj")
        .args(["bookmark", "list", "-T", "name ++ \"\\n\""])
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .find(|name| name.starts_with(&prefix))
        .map(str::to_owned)
}

struct LiveExecution {
    repository: Repository,
}

impl LiveExecution {
    fn new(repository: Repository) -> Self {
        Self { repository }
    }
}

impl OrchestrationExecution for LiveExecution {
    type Revision = StateRevision<DispatchGroupState>;
    type Claim = Reservation;

    fn load_group(
        &mut self,
        parent: &TicketId,
    ) -> Result<LoadedOrchestration<Self::Revision>, OrchestrationError> {
        let repository = self.repository.state_store().dispatch_group(parent.clone());
        let versioned = match repository
            .load()
            .map_err(|error| OrchestrationError::Execution(error.to_string()))?
        {
            Loaded::Present(versioned) => versioned,
            Loaded::Missing => {
                return Err(OrchestrationError::MissingGroup {
                    parent: parent.clone(),
                });
            }
        };
        let (state, revision) = versioned.into_parts();
        Ok(LoadedOrchestration {
            group: DispatchGroup::from_state(state)?,
            revision,
        })
    }

    fn workers(&mut self) -> Result<Vec<WorkerObservation>, OrchestrationError> {
        Ok(self
            .repository
            .worker_pool()
            .reconcile_runs()
            .workers()
            .iter()
            .map(|worker| WorkerObservation {
                worker: worker.worker_id().clone(),
                status: worker.status(),
                ticket: worker.ticket().map(str::to_owned),
                branch: worker.branch_name().map(str::to_owned),
                error: worker.error().map(str::to_owned),
            })
            .collect())
    }

    fn revalidate_branches(
        &mut self,
        group: &mut DispatchGroup,
    ) -> Result<Vec<BranchRepair>, OrchestrationError> {
        Ok(revalidate_branches(self.repository.root(), group))
    }

    fn save_group(
        &mut self,
        expected: &Self::Revision,
        group: &DispatchGroup,
    ) -> Result<Self::Revision, OrchestrationError> {
        let parent = group.state().parent.clone();
        let outcome = self
            .repository
            .state_store()
            .dispatch_group(parent.clone())
            .commit(
                Expected::Match(expected.clone()),
                StateChange::Replace(group.state().clone()),
            )
            .map_err(|error| OrchestrationError::Execution(error.to_string()))?;
        match outcome {
            CommitOutcome::Applied(Loaded::Present(versioned)) => Ok(versioned.revision().clone()),
            CommitOutcome::Conflict(_) => Err(OrchestrationError::ConcurrentChange { parent }),
            CommitOutcome::Applied(Loaded::Missing) => Err(OrchestrationError::Execution(
                "Dispatch Group replacement unexpectedly removed state".to_owned(),
            )),
        }
    }

    fn reset_worker(&mut self, worker: &WorkerId) -> Result<(), OrchestrationError> {
        let restoration = WorkerActions::new(self.repository.clone())
            .reset(worker)
            .map_err(|error| OrchestrationError::Execution(error.to_string()))?
            .into_restoration();
        if let WorkspaceRestoration::Pending(handle) = restoration {
            handle
                .wait()
                .map_err(|error| OrchestrationError::Execution(error.to_string()))?;
        }
        Ok(())
    }

    fn claim(
        &mut self,
        request: &DirectDispatchRequest,
    ) -> Result<Option<Self::Claim>, OrchestrationError> {
        match self.repository.direct_dispatch().reserve(request) {
            Ok(claim) => Ok(Some(claim)),
            Err(DirectDispatchError::WorkerPool(
                WorkerPoolError::NoIdleWorkers { .. } | WorkerPoolError::CapacityShortage(_),
            )) => Ok(None),
            Err(error) => Err(OrchestrationError::Execution(error.to_string())),
        }
    }

    fn claimed_worker(claim: &Self::Claim) -> &WorkerId {
        claim.worker_id()
    }

    fn release(&mut self, claim: Self::Claim) -> Result<(), OrchestrationError> {
        claim
            .release()
            .map_err(|error| OrchestrationError::Execution(error.to_string()))
    }

    fn launch(
        &mut self,
        claim: Self::Claim,
        request: &DirectDispatchRequest,
    ) -> Result<(), LaunchFailure> {
        self.repository
            .direct_dispatch()
            .dispatch_reserved(claim, request)
            .map(|_| ())
            .map_err(|error| LaunchFailure {
                compensated: !matches!(error, DirectDispatchError::ReservationRelease { .. }),
                detail: error.to_string(),
            })
    }

    fn now(&mut self) -> Result<WireTimestamp, OrchestrationError> {
        current_timestamp().map_err(|error| OrchestrationError::Execution(error.to_string()))
    }
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

    /// Reserves a placeholder while discovering children, then releases it before
    /// persisting a real group or hands it to the Parent fallback Run.
    pub fn discover<Q: TicketQuery>(
        &self,
        request: &OrchestrationRequest,
        parent: &ParentTicket,
        discovery: &TicketDiscovery<Q>,
        repository: &RepositoryIdentity,
        gh_repo: impl Into<String>,
    ) -> Result<OrchestrationStart, OrchestrationError> {
        let title = TicketTitle::parse(parent.id().as_str())
            .map_err(|error| OrchestrationError::Execution(error.to_string()))?;
        let status = TicketStatus::parse("Todo")
            .map_err(|error| OrchestrationError::Execution(error.to_string()))?;
        let mut placeholder = DirectDispatchRequest::new(
            Ticket::new(parent.id().clone(), title, status),
            RunMode::Background,
        );
        if let Some(model) = request.model() {
            placeholder = placeholder.with_model(model);
        }
        let reservation = match self.repository.direct_dispatch().reserve(&placeholder) {
            Ok(reservation) => reservation,
            Err(error) => return Err(OrchestrationError::Execution(error.to_string())),
        };
        let graph = match discovery.dependency_graph(parent, repository) {
            Ok(graph) => graph,
            Err(error) => {
                let primary = error.to_string();
                return match reservation.release() {
                    Ok(()) => Err(OrchestrationError::Execution(primary)),
                    Err(cleanup) => Err(OrchestrationError::DiscoveryCleanup {
                        primary,
                        cleanup: cleanup.to_string(),
                    }),
                };
            }
        };
        if graph.sub_issues().is_empty() {
            return self
                .repository
                .direct_dispatch()
                .dispatch_reserved(reservation, &placeholder)
                .map(|success| OrchestrationStart::Direct(Box::new(success)))
                .map_err(|error| OrchestrationError::Execution(error.to_string()));
        }
        reservation
            .release()
            .map_err(|error| OrchestrationError::Execution(error.to_string()))?;
        let mut group_options =
            DispatchGroupOptions::new(request.model().unwrap_or_default().to_owned());
        group_options.agent = Some(WireAgent::new(request.agent_runtime().as_str()));
        let options = DispatchGroupBuildOptions::new(
            current_timestamp()
                .map_err(|error| OrchestrationError::Execution(error.to_string()))?,
            gh_repo,
            group_options,
        );
        let group = DispatchGroup::from_dependency_graph(&graph, options)?;
        let state = group.clone().into_state();
        match self
            .repository
            .state_store()
            .dispatch_group(parent.id().clone())
            .commit(Expected::Missing, StateChange::Replace(state))
            .map_err(|error| OrchestrationError::Execution(error.to_string()))?
        {
            CommitOutcome::Applied(_) => Ok(OrchestrationStart::Group),
            CommitOutcome::Conflict(_) => Err(OrchestrationError::ConcurrentChange {
                parent: parent.id().clone(),
            }),
        }
    }

    /// Prepares a Parent Ticket by resuming existing state or discovering and
    /// persisting a new Dispatch Group. Persistence and Repository identity
    /// policy stay behind this deep interface.
    pub fn prepare<Q: TicketQuery>(
        &self,
        request: &OrchestrationRequest,
        parent: &ParentTicket,
        discovery: &TicketDiscovery<Q>,
    ) -> Result<OrchestrationPreparation, OrchestrationError> {
        let group_repository = self
            .repository
            .state_store()
            .dispatch_group(parent.id().clone());
        match group_repository
            .load()
            .map_err(|error| OrchestrationError::Execution(error.to_string()))?
        {
            Loaded::Present(versioned) => {
                let group = DispatchGroup::from_state(versioned.value)?;
                return Ok(OrchestrationPreparation {
                    maximum_wave: group.maximum_wave_size(),
                    start: OrchestrationStart::Group,
                    resumed: true,
                });
            }
            Loaded::Missing => {}
        }
        let pool = self.repository.worker_pool().snapshot();
        let pool = pool
            .pool()
            .ok_or_else(|| OrchestrationError::Execution("Worker Pool is missing".to_owned()))?;
        let identity = RepositoryIdentity::parse(pool.gh_repo())
            .map_err(|error| OrchestrationError::Execution(error.to_string()))?;
        let start = self.discover(request, parent, discovery, &identity, pool.gh_repo())?;
        let maximum_wave = match &start {
            OrchestrationStart::Group => match group_repository
                .load()
                .map_err(|error| OrchestrationError::Execution(error.to_string()))?
            {
                Loaded::Present(versioned) => {
                    DispatchGroup::from_state(versioned.value)?.maximum_wave_size()
                }
                Loaded::Missing => 0,
            },
            OrchestrationStart::Direct(_) => 0,
        };
        Ok(OrchestrationPreparation {
            start,
            resumed: false,
            maximum_wave,
        })
    }

    /// Reconciles one persisted group and dispatches every currently ready Ticket that fits.
    ///
    /// State transitions are committed before events are delivered. Capacity shortage is
    /// reported as an event and leaves the Ticket pending for a later advance.
    pub fn advance_once(
        &self,
        request: &OrchestrationRequest,
        mut observer: impl FnMut(OrchestrationEvent),
    ) -> Result<Option<OrchestrationSummary>, OrchestrationError> {
        let _lock = self.acquire_lock(request.parent())?;
        self.advance_once_unlocked(request, &mut observer)
    }

    fn advance_once_unlocked(
        &self,
        request: &OrchestrationRequest,
        observer: &mut impl FnMut(OrchestrationEvent),
    ) -> Result<Option<OrchestrationSummary>, OrchestrationError> {
        let mut execution = LiveExecution::new(self.repository.clone());
        run_with_execution(request, &mut execution, observer)?;
        let loaded = execution.load_group(request.parent())?;
        if loaded.group.is_terminal() {
            let summary = OrchestrationSummary {
                parent: request.parent().clone(),
                counts: loaded.group.status_counts(),
                direct_worker: None,
            };
            observer(OrchestrationEvent::Terminal(summary.clone()));
            Ok(Some(summary))
        } else {
            Ok(None)
        }
    }

    fn acquire_lock(&self, parent: &TicketId) -> Result<File, OrchestrationError> {
        let directory = self.repository.root().join(".jj/pool");
        fs::create_dir_all(&directory)
            .map_err(|error| OrchestrationError::Execution(error.to_string()))?;
        let path = directory.join(format!(
            "orchestrate-{}.lock",
            parent.as_str().to_ascii_lowercase()
        ));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| OrchestrationError::Execution(error.to_string()))?;
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(file),
            Err(error) if error == rustix::io::Errno::WOULDBLOCK => {
                Err(OrchestrationError::AlreadyRunning {
                    parent: parent.clone(),
                })
            }
            Err(error) => Err(OrchestrationError::Execution(error.to_string())),
        }
    }

    /// Watches one persisted group until it reaches a terminal state.
    pub fn run(
        &self,
        request: &OrchestrationRequest,
        options: &OrchestrationOptions,
        mut observer: impl FnMut(OrchestrationEvent),
    ) -> Result<OrchestrationSummary, OrchestrationError> {
        let _lock = self.acquire_lock(request.parent())?;
        let mut execution = LiveExecution::new(self.repository.clone());
        execution.load_group(request.parent())?;
        observer(OrchestrationEvent::Started {
            parent: request.parent().clone(),
            resumed: true,
        });
        let mut cycles = 0;
        loop {
            if options.max_cycles.is_some_and(|limit| cycles >= limit) {
                return Err(OrchestrationError::PollingExhausted {
                    parent: request.parent().clone(),
                    cycles,
                });
            }
            cycles += 1;
            if let Some(summary) = self.advance_once_unlocked(request, &mut observer)? {
                return Ok(summary);
            }
            std::thread::sleep(options.poll_interval.min(Duration::from_secs(5)));
        }
    }

    /// Detached-process-compatible entrypoint with the same durable semantics as foreground watch.
    pub fn run_detached(
        &self,
        request: &OrchestrationRequest,
        options: &OrchestrationOptions,
        observer: impl FnMut(OrchestrationEvent),
    ) -> Result<OrchestrationSummary, OrchestrationError> {
        self.run(request, options, observer)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, VecDeque};

    use super::*;
    use crate::{DispatchGroupOptions, DispatchGroupState, SubIssueState, WireStatus};

    struct FakeClaim {
        worker: WorkerId,
        ticket: TicketId,
    }

    struct FakeExecution {
        group: DispatchGroup,
        revision: u64,
        workers: Vec<WorkerObservation>,
        calls: Vec<String>,
        fail_next_save: bool,
        reset_fails: bool,
        available_workers: VecDeque<WorkerId>,
        failed_launches: BTreeSet<TicketId>,
        launched_requests: Vec<DirectDispatchRequest>,
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
                    error: None,
                }],
                calls: Vec::new(),
                fail_next_save: false,
                reset_fails: false,
                available_workers: VecDeque::new(),
                failed_launches: BTreeSet::new(),
                launched_requests: Vec::new(),
            }
        }

        fn failed_attempt(retries: i64) -> Self {
            let ticket = TicketId::parse("ENG-101").expect("Ticket");
            let worker = WorkerId::parse("worker-01").expect("Worker");
            let mut state = DispatchGroupState::new(
                TicketId::parse("ENG-100").expect("Parent Ticket"),
                WireTimestamp::new("2026-08-04T12:00:00Z"),
                "owner/repo",
                DispatchGroupOptions::new(""),
            );
            let mut issue = SubIssueState::new(
                "Build the foundation",
                WireStatus::new("dispatched"),
                Vec::new(),
            );
            issue.worker = Some(worker.clone());
            issue.dispatched_at = Some(WireTimestamp::new("2026-08-04T12:01:00Z"));
            issue.retries = retries;
            state.sub_issues.insert(ticket.clone(), issue);
            Self {
                group: DispatchGroup::from_state(state).expect("valid failed group"),
                revision: 0,
                workers: vec![WorkerObservation {
                    worker,
                    status: WorkerStatus::Failed,
                    ticket: Some(ticket.as_str().to_owned()),
                    branch: None,
                    error: Some("build failed".to_owned()),
                }],
                calls: Vec::new(),
                fail_next_save: false,
                reset_fails: false,
                available_workers: VecDeque::new(),
                failed_launches: BTreeSet::new(),
                launched_requests: Vec::new(),
            }
        }

        fn independent_pending(tickets: &[&str], workers: &[&str]) -> Self {
            let mut state = DispatchGroupState::new(
                TicketId::parse("ENG-100").expect("Parent Ticket"),
                WireTimestamp::new("2026-08-04T12:00:00Z"),
                "owner/repo",
                DispatchGroupOptions::new(""),
            );
            for ticket in tickets {
                state.sub_issues.insert(
                    TicketId::parse(*ticket).expect("Ticket"),
                    SubIssueState::new(
                        format!("Implement {ticket}"),
                        WireStatus::new("pending"),
                        Vec::new(),
                    ),
                );
            }
            let available_workers = workers
                .iter()
                .map(|worker| WorkerId::parse(*worker).expect("Worker"))
                .collect();
            Self {
                group: DispatchGroup::from_state(state).expect("valid pending group"),
                revision: 0,
                workers: Vec::new(),
                calls: Vec::new(),
                fail_next_save: false,
                reset_fails: false,
                available_workers,
                failed_launches: BTreeSet::new(),
                launched_requests: Vec::new(),
            }
        }

        fn stacked_pending() -> Self {
            let blocker = TicketId::parse("ENG-101").expect("Blocker Ticket");
            let dependent = TicketId::parse("ENG-102").expect("dependent Ticket");
            let mut state = DispatchGroupState::new(
                TicketId::parse("ENG-100").expect("Parent Ticket"),
                WireTimestamp::new("2026-08-04T12:00:00Z"),
                "owner/repo",
                DispatchGroupOptions::new(""),
            );
            let mut delivered =
                SubIssueState::new("Build the foundation", WireStatus::new("done"), Vec::new());
            delivered.branch = Some("adam/eng-101-foundation".to_owned());
            delivered.completed_at = Some(WireTimestamp::new("2026-08-04T12:01:00Z"));
            state.sub_issues.insert(blocker.clone(), delivered);
            state.sub_issues.insert(
                dependent,
                SubIssueState::new(
                    "Use the foundation",
                    WireStatus::new("pending"),
                    vec![blocker],
                ),
            );
            Self {
                group: DispatchGroup::from_state(state).expect("valid stacked group"),
                revision: 0,
                workers: Vec::new(),
                calls: Vec::new(),
                fail_next_save: false,
                reset_fails: false,
                available_workers: VecDeque::from([WorkerId::parse("worker-01").expect("Worker")]),
                failed_launches: BTreeSet::new(),
                launched_requests: Vec::new(),
            }
        }

        fn fail_next_save(&mut self) {
            self.fail_next_save = true;
        }

        fn fail_reset(&mut self) {
            self.reset_fails = true;
        }

        fn fail_launch(&mut self, ticket: &str) {
            self.failed_launches
                .insert(TicketId::parse(ticket).expect("failed launch Ticket"));
        }
    }

    impl OrchestrationExecution for FakeExecution {
        type Revision = u64;
        type Claim = FakeClaim;

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

        fn revalidate_branches(
            &mut self,
            _group: &mut DispatchGroup,
        ) -> Result<Vec<BranchRepair>, OrchestrationError> {
            Ok(Vec::new())
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
            if self.reset_fails {
                return Err(OrchestrationError::Execution(
                    "Worker Reset failed".to_owned(),
                ));
            }
            Ok(())
        }

        fn claim(
            &mut self,
            request: &DirectDispatchRequest,
        ) -> Result<Option<Self::Claim>, OrchestrationError> {
            let ticket = request.ticket().id().clone();
            self.calls.push(format!("claim:{ticket}"));
            Ok(self
                .available_workers
                .pop_front()
                .map(|worker| FakeClaim { worker, ticket }))
        }

        fn claimed_worker(claim: &Self::Claim) -> &WorkerId {
            &claim.worker
        }

        fn release(&mut self, claim: Self::Claim) -> Result<(), OrchestrationError> {
            self.calls
                .push(format!("release:{}:{}", claim.worker, claim.ticket));
            Ok(())
        }

        fn launch(
            &mut self,
            claim: Self::Claim,
            request: &DirectDispatchRequest,
        ) -> Result<(), LaunchFailure> {
            let ticket = request.ticket().id();
            self.calls.push(format!("launch:{}:{ticket}", claim.worker));
            self.launched_requests.push(request.clone());
            if self.failed_launches.contains(ticket) {
                return Err(LaunchFailure {
                    detail: "launch failed".to_owned(),
                    compensated: true,
                });
            }
            Ok(())
        }

        fn now(&mut self) -> Result<WireTimestamp, OrchestrationError> {
            Ok(WireTimestamp::new("2026-08-04T12:02:00Z"))
        }
    }

    #[test]
    fn stacked_dependency_context_reaches_background_direct_dispatch() {
        let request = OrchestrationRequest::new(
            TicketId::parse("ENG-100").expect("Parent Ticket"),
            AgentRuntime::Claude,
        )
        .with_model("opus");
        let mut execution = FakeExecution::stacked_pending();

        run_with_execution(&request, &mut execution, &mut |_| {})
            .expect("dispatch stacked dependent");

        let launched = execution
            .launched_requests
            .first()
            .expect("one launched request");
        let dependency = launched
            .dependency_context()
            .expect("stacked dependency context");
        assert_eq!(launched.mode(), RunMode::Background);
        assert_eq!(launched.model(), Some("opus"));
        assert_eq!(
            dependency.base_revisions(),
            &["adam/eng-101-foundation".to_owned()]
        );
        assert_eq!(dependency.pull_request_base(), "adam/eng-101-foundation");
        assert!(dependency.description().contains("ENG-101"));
    }

    #[test]
    fn ready_tickets_launch_in_stable_order_until_capacity_runs_out() {
        let request = OrchestrationRequest::new(
            TicketId::parse("ENG-100").expect("Parent Ticket"),
            AgentRuntime::Claude,
        );
        let mut execution = FakeExecution::independent_pending(
            &["ENG-103", "ENG-101", "ENG-102"],
            &["worker-01", "worker-02"],
        );
        let mut events = Vec::new();

        run_with_execution(&request, &mut execution, &mut |event| events.push(event))
            .expect("dispatch available wave");

        assert_eq!(
            execution.calls,
            vec![
                "workers".to_owned(),
                "claim:ENG-101".to_owned(),
                "save:r0:ENG-101:dispatched".to_owned(),
                "launch:worker-01:ENG-101".to_owned(),
                "claim:ENG-102".to_owned(),
                "save:r1:ENG-101:dispatched".to_owned(),
                "launch:worker-02:ENG-102".to_owned(),
                "claim:ENG-103".to_owned(),
            ]
        );
        assert!(matches!(
            events.as_slice(),
            [
                OrchestrationEvent::Dispatched { ticket: first, .. },
                OrchestrationEvent::Dispatched { ticket: second, .. },
                OrchestrationEvent::WaitingForCapacity { ticket: waiting }
            ] if first.as_str() == "ENG-101"
                && second.as_str() == "ENG-102"
                && waiting.as_str() == "ENG-103"
        ));
    }

    #[test]
    fn compensated_launch_failure_returns_the_ticket_to_pending_without_a_retry() {
        let request = OrchestrationRequest::new(
            TicketId::parse("ENG-100").expect("Parent Ticket"),
            AgentRuntime::Claude,
        );
        let mut execution = FakeExecution::independent_pending(&["ENG-101"], &["worker-01"]);
        execution.fail_launch("ENG-101");
        let mut events = Vec::new();

        run_with_execution(&request, &mut execution, &mut |event| events.push(event))
            .expect("compensate failed launch");

        assert_eq!(
            execution.calls,
            vec![
                "workers".to_owned(),
                "claim:ENG-101".to_owned(),
                "save:r0:ENG-101:dispatched".to_owned(),
                "launch:worker-01:ENG-101".to_owned(),
                "save:r1:ENG-101:pending".to_owned(),
            ]
        );
        let issue = execution
            .group
            .state()
            .sub_issues
            .get(&TicketId::parse("ENG-101").expect("Ticket"))
            .expect("compensated Ticket");
        assert_eq!(issue.status.as_str(), "pending");
        assert_eq!(issue.retries, 0);
        assert!(matches!(
            events.as_slice(),
            [OrchestrationEvent::LaunchFailed { .. }]
        ));
    }

    #[test]
    fn reservation_is_released_when_assignment_persistence_conflicts() {
        let request = OrchestrationRequest::new(
            TicketId::parse("ENG-100").expect("Parent Ticket"),
            AgentRuntime::Claude,
        );
        let mut execution = FakeExecution::independent_pending(&["ENG-101"], &["worker-01"]);
        execution.fail_next_save();
        let mut events = Vec::new();

        let error = run_with_execution(&request, &mut execution, &mut |event| events.push(event))
            .expect_err("stale assignment must stop");

        assert!(matches!(error, OrchestrationError::ConcurrentChange { .. }));
        assert_eq!(
            execution.calls,
            vec![
                "workers".to_owned(),
                "claim:ENG-101".to_owned(),
                "save:r0:ENG-101:dispatched".to_owned(),
                "release:worker-01:ENG-101".to_owned(),
            ]
        );
        assert!(events.is_empty());
    }

    #[test]
    fn first_failed_run_resets_before_persisting_one_retry() {
        let request = OrchestrationRequest::new(
            TicketId::parse("ENG-100").expect("Parent Ticket"),
            AgentRuntime::Claude,
        );
        let mut execution = FakeExecution::failed_attempt(0);
        let mut events = Vec::new();

        run_with_execution(&request, &mut execution, &mut |event| events.push(event))
            .expect("retry first failure");

        assert_eq!(
            execution.calls,
            vec![
                "workers".to_owned(),
                "reset:worker-01".to_owned(),
                "save:r0:ENG-101:pending".to_owned(),
                "claim:ENG-101".to_owned(),
            ]
        );
        let issue = execution
            .group
            .state()
            .sub_issues
            .get(&TicketId::parse("ENG-101").expect("Ticket"))
            .expect("retried Ticket");
        assert_eq!(issue.retries, 1);
        assert!(matches!(
            events.as_slice(),
            [
                OrchestrationEvent::Retrying { attempt: 2, .. },
                OrchestrationEvent::WaitingForCapacity { .. }
            ]
        ));
    }

    #[test]
    fn reset_failure_leaves_the_first_failed_run_dispatched() {
        let request = OrchestrationRequest::new(
            TicketId::parse("ENG-100").expect("Parent Ticket"),
            AgentRuntime::Claude,
        );
        let mut execution = FakeExecution::failed_attempt(0);
        execution.fail_reset();
        let mut events = Vec::new();

        let error = run_with_execution(&request, &mut execution, &mut |event| events.push(event))
            .expect_err("Reset failure must stop retry");

        assert!(matches!(error, OrchestrationError::Execution(_)));
        assert_eq!(
            execution.calls,
            vec!["workers".to_owned(), "reset:worker-01".to_owned()]
        );
        assert_eq!(
            execution
                .group
                .state()
                .sub_issues
                .get(&TicketId::parse("ENG-101").expect("Ticket"))
                .expect("failed Ticket")
                .status
                .as_str(),
            "dispatched"
        );
        assert!(events.is_empty());
    }

    #[test]
    fn second_failed_run_is_persisted_terminal_without_another_reset() {
        let request = OrchestrationRequest::new(
            TicketId::parse("ENG-100").expect("Parent Ticket"),
            AgentRuntime::Claude,
        );
        let mut execution = FakeExecution::failed_attempt(1);
        let mut events = Vec::new();

        run_with_execution(&request, &mut execution, &mut |event| events.push(event))
            .expect("record exhausted retry");

        assert_eq!(
            execution.calls,
            vec!["workers".to_owned(), "save:r0:ENG-101:failed".to_owned(),]
        );
        assert!(matches!(
            events.as_slice(),
            [OrchestrationEvent::Failed { detail: Some(detail), .. }] if detail == "build failed"
        ));
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
