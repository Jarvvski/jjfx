//! Worker Pool lifecycle operations and snapshots over compatible state repositories.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use jiff::{RoundMode, Timestamp, TimestampRound, Unit};
use thiserror::Error;

use crate::engine::{
    AgentRuntime, AgentRuntimeProfile, CommitOutcome, DispatchExclusions, Expected, Loaded,
    PoolCostSnapshot, PoolState, Repository, RunConclusion, RunCost, RunHealth, RunProgress,
    RunRecord, RunRecordConclusion, RunRecordUsage, RunResult, RunResultSource, SessionOutcome,
    StateChange, StateError, StateRevision, StuckWindow, WireStatus, WireTimestamp, WorkerId,
    WorkerState, WorkerWorkspaceError,
};

/// Bounded retries for a Reset losing the Worker revision to its own Run's
/// natural finalization.
const CLEAR_RUN_ATTEMPTS: usize = 3;

/// A number of reusable Worker slots, including an empty Pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PoolCapacity(usize);

impl PoolCapacity {
    /// Creates a capacity that fits the persisted signed 64-bit Pool size.
    pub fn new(value: usize) -> Result<Self, PoolCapacityError> {
        i64::try_from(value).map_err(|_| PoolCapacityError { value })?;
        Ok(Self(value))
    }

    /// Returns the number of Worker slots.
    pub fn as_usize(self) -> usize {
        self.0
    }
}

/// A Worker Pool capacity too large for the compatible persisted size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("Worker Pool capacity {value} exceeds the compatible signed 64-bit size")]
pub struct PoolCapacityError {
    value: usize,
}

/// Exact idle-capacity shortage reported before a batch Reservation writes state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("Worker Pool has {available} idle Workers but {requested} are required")]
pub struct CapacityShortage {
    requested: usize,
    available: usize,
}

impl CapacityShortage {
    /// Creates a shortage from the locked requested and available counts.
    pub const fn new(requested: usize, available: usize) -> Self {
        Self {
            requested,
            available,
        }
    }

    /// Returns the number of requested Worker slots.
    pub const fn requested(self) -> usize {
        self.requested
    }

    /// Returns the number of idle Worker slots observed under lock.
    pub const fn available(self) -> usize {
        self.available
    }

    /// Returns the exact additional idle capacity required at observation time.
    pub const fn gap(self) -> usize {
        self.requested.saturating_sub(self.available)
    }
}

/// The result of changing Worker Pool capacity or named membership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolResize {
    capacity: PoolCapacity,
    added_workers: Vec<WorkerId>,
    removed_workers: Vec<WorkerId>,
}

impl PoolResize {
    /// Returns the resulting Pool capacity.
    pub fn capacity(&self) -> PoolCapacity {
        self.capacity
    }

    /// Returns Workers provisioned by this operation in Pool order.
    pub fn added_workers(&self) -> &[WorkerId] {
        &self.added_workers
    }

    /// Returns Workers detached by this operation in their prior Pool order.
    pub fn removed_workers(&self) -> &[WorkerId] {
        &self.removed_workers
    }
}

/// Capacity reserved for one Ticket before an Agent Runtime starts.
#[derive(Debug, PartialEq, Eq)]
pub struct Reservation {
    worker_id: WorkerId,
    ticket: String,
    profile: AgentRuntimeProfile,
    repository: Repository,
    worker_revision: StateRevision<WorkerState>,
    rollback: WorkerState,
}

pub(crate) struct AvailableReservations {
    pub(crate) claimed: Vec<(usize, Reservation)>,
    pub(crate) unavailable: Vec<usize>,
}

pub(crate) enum PidPersistence {
    Persisted(StateRevision<WorkerState>),
    Missing,
    Conflict,
}

pub(crate) enum RunFinalization {
    Applied,
    Stale,
}

/// The persisted Run a Reset intends to abandon.
///
/// The identity fields are the Run-owned values a natural finalization leaves
/// untouched, so a Reset can tell "my Run finished while I was terminating it"
/// apart from "a different Run now owns this Worker".
pub(crate) struct RunTarget {
    revision: StateRevision<WorkerState>,
    runtime: Option<AgentRuntime>,
    pid: Option<i64>,
    ticket: Option<String>,
    started_at: Option<WireTimestamp>,
    log_file: Option<String>,
}

impl RunTarget {
    fn new(state: &WorkerState, revision: StateRevision<WorkerState>) -> Self {
        Self {
            revision,
            runtime: state.agent.as_ref().and_then(AgentRuntime::parse),
            pid: state.pid,
            ticket: state.ticket.clone(),
            started_at: state.started_at.clone(),
            log_file: state.log_file.clone(),
        }
    }

    pub(crate) fn runtime(&self) -> Option<AgentRuntime> {
        self.runtime
    }

    pub(crate) fn ticket(&self) -> Option<&str> {
        self.ticket.as_deref()
    }

    pub(crate) fn pid(&self) -> Option<u32> {
        self.pid.and_then(|pid| u32::try_from(pid).ok())
    }

    fn is_same_run(&self, state: &WorkerState) -> bool {
        self.pid == state.pid
            && self.ticket == state.ticket
            && self.started_at == state.started_at
            && self.log_file == state.log_file
    }
}

/// The outcome of returning a Worker to idle after its Run was abandoned.
pub(crate) enum RunClearing {
    Cleared,
    AlreadyIdle,
    Superseded,
}

impl Reservation {
    /// Returns the Worker assigned to this Reservation.
    pub fn worker_id(&self) -> &WorkerId {
        &self.worker_id
    }

    /// Returns the Ticket assigned to this Reservation.
    pub fn ticket(&self) -> &str {
        &self.ticket
    }

    /// Returns the Agent Runtime persisted for this Run.
    pub fn agent_runtime(&self) -> AgentRuntime {
        self.profile.runtime()
    }

    /// Returns the complete runtime and model profile persisted for this Run.
    pub fn profile(&self) -> &AgentRuntimeProfile {
        &self.profile
    }

    pub(crate) fn repository(&self) -> &Repository {
        &self.repository
    }

    pub(crate) fn persist_pid(&self, pid: u32) -> Result<PidPersistence, StateError> {
        match self.repository.state_store().persist_reserved_pid(
            &self.worker_id,
            self.worker_revision.clone(),
            pid,
        )? {
            CommitOutcome::Applied(Loaded::Present(versioned)) => {
                Ok(PidPersistence::Persisted(versioned.revision().clone()))
            }
            CommitOutcome::Applied(Loaded::Missing) | CommitOutcome::Conflict(Loaded::Missing) => {
                Ok(PidPersistence::Missing)
            }
            CommitOutcome::Conflict(Loaded::Present(_)) => Ok(PidPersistence::Conflict),
        }
    }

    pub(crate) fn finalize(
        &self,
        revision: StateRevision<WorkerState>,
        result: &RunResult,
    ) -> Result<RunFinalization, WorkerPoolError> {
        let worker_state = self.repository.state_store().worker(self.worker_id.clone());
        let loaded = match worker_state.load()? {
            Loaded::Present(versioned) => versioned,
            Loaded::Missing => {
                return Err(WorkerPoolError::WorkerStateMissing {
                    worker: self.worker_id.clone(),
                });
            }
        };
        if loaded.value.status.as_str() != "busy" {
            return Ok(RunFinalization::Stale);
        }
        let mut state = loaded.value;
        apply_run_result(&mut state, result)?;
        let record = run_record(&self.repository, &self.worker_id, &state, result);
        match worker_state.commit(Expected::Match(revision), StateChange::Replace(state))? {
            CommitOutcome::Applied(_) => {
                self.repository.state_store().runs().append(&record)?;
                Ok(RunFinalization::Applied)
            }
            CommitOutcome::Conflict(_) => Ok(RunFinalization::Stale),
        }
    }

    pub(crate) fn release(&self) -> Result<(), WorkerPoolError> {
        let worker_state = self.repository.state_store().worker(self.worker_id.clone());
        match worker_state.commit(
            Expected::Match(self.worker_revision.clone()),
            StateChange::Replace(self.rollback.clone()),
        )? {
            CommitOutcome::Applied(_) => Ok(()),
            CommitOutcome::Conflict(_) => Err(WorkerPoolError::ReleaseConflict {
                worker: self.worker_id.clone(),
            }),
        }
    }
}

/// Errors from Worker Pool lifecycle and Reservation operations.
#[derive(Debug, Error)]
pub enum WorkerPoolError {
    #[error("cannot load Worker Pool state: {0}")]
    State(#[from] StateError),
    #[error("Worker Pool state has invalid size {0}")]
    InvalidSize(i64),
    #[error("cannot change Worker Pool membership because Workers are busy: {workers:?}")]
    WorkersBusy { workers: Vec<WorkerId> },
    #[error("Worker Pool mutation conflicted with another process")]
    Conflict,
    #[error("no idle Worker is available for Ticket {ticket} (available: {available})")]
    NoIdleWorkers { ticket: String, available: usize },
    #[error(transparent)]
    CapacityShortage(#[from] CapacityShortage),
    #[error("Worker {worker} is not a member of the Worker Pool")]
    WorkerNotInPool { worker: WorkerId },
    #[error("Worker {worker} is not idle")]
    WorkerNotIdle { worker: WorkerId },
    #[error("Worker {worker} is open in a terminal and not eligible for Dispatch")]
    WorkerNotEligible { worker: WorkerId },
    #[error("Worker {worker} state is missing")]
    WorkerStateMissing { worker: WorkerId },
    #[error("Worker {worker} changed before its Reservation could be released")]
    ReleaseConflict { worker: WorkerId },
    #[error("Worker {worker} kept changing while its Run was being reset")]
    ResetConflict { worker: WorkerId },
    #[error("cannot reconcile Worker {worker}: {source}")]
    Reconciliation {
        worker: WorkerId,
        source: StateError,
    },
    #[error("{message}")]
    InvalidAgentRuntime { message: String },
    #[error("cannot discover GitHub repository: {0}")]
    RepositoryDiscovery(String),
    #[error("cannot create a Worker timestamp: {0}")]
    Timestamp(String),
    #[error("cannot generate a Worker ID: {0}")]
    WorkerId(String),
    #[error("cannot provision Worker {worker}: {source}")]
    Provision {
        worker: WorkerId,
        source: WorkerWorkspaceError,
    },
    #[error("Worker Pool compensation failed: {0}")]
    Compensation(String),
    #[error("Worker Pool membership changed but detached Worker cleanup failed: {0}")]
    Cleanup(String),
    #[error("Worker Pool destruction left residual Workers {workers:?}: {details}")]
    DestroyCleanup {
        workers: Vec<WorkerId>,
        details: String,
    },
}

/// The deep Worker Pool lifecycle module for one repository.
#[derive(Debug, Clone)]
pub struct WorkerPool {
    repository: Repository,
}

impl WorkerPool {
    pub(crate) fn new(repository: Repository) -> Self {
        Self { repository }
    }

    /// Reads a compatible Worker Pool snapshot without changing any file.
    pub fn snapshot(&self) -> WorkerPoolSnapshot {
        self.repository.read_worker_pool_snapshot()
    }

    /// Reconciles observable Worker Run state and returns the resulting snapshot.
    ///
    /// A Worker with a dead recorded PID is marked failed using the compatible
    /// unexpected-exit fallback. Missing, malformed, and concurrently replaced
    /// Worker state remains observable through the returned snapshot.
    pub fn reconcile_runs(&self) -> WorkerPoolSnapshot {
        let before = self.snapshot();
        let mut diagnostics = Vec::new();
        for worker in before.workers() {
            let worker_id = worker.worker_id();
            // A Pool member whose Worker Workspace directory is gone can never
            // run: `prepare_for_dispatch` refuses it. Report that rather than let
            // it pass for a healthy idle Worker, so a frontend can say so and an
            // operator can dismiss it.
            let workspace =
                crate::engine::workspace::worker_path(self.repository.root(), worker_id);
            if !workspace.is_dir() {
                diagnostics.push(diagnostic(
                    self.repository.root(),
                    SnapshotDiagnosticKind::MissingWorkspace,
                    Some(worker_id.clone()),
                    format!(
                        "Worker {worker_id} Workspace is missing at {}",
                        workspace.display()
                    ),
                ));
            }
            if let Err(error) = self.reconcile_worker(worker_id) {
                diagnostics.push(diagnostic(
                    self.repository.root(),
                    SnapshotDiagnosticKind::ReconciliationFailed,
                    Some(worker_id.clone()),
                    error,
                ));
            }
        }
        let mut after = self.snapshot();
        after.diagnostics.extend(diagnostics);
        after
    }

    fn reconcile_worker(&self, worker: &WorkerId) -> Result<(), WorkerPoolError> {
        let repository = self.repository.state_store().worker(worker.clone());
        let loaded = match repository.load().map_err(WorkerPoolError::State)? {
            Loaded::Missing => return Ok(()),
            Loaded::Present(versioned) => versioned,
        };
        if loaded.value.status.as_str() != "busy" {
            return Ok(());
        }
        let runtime = loaded.value.agent.as_ref().and_then(AgentRuntime::parse);
        let result = if let Some(removed) = removed_worker_runtime(&loaded.value) {
            // The runtime this Run was launched under has been removed, so no
            // process can still be its Run and nothing can finish or resume it.
            // Finalize it rather than leave the Worker stranded `busy` forever.
            RunResult::failed(removed)
        } else if runtime == Some(AgentRuntime::OpenCode) {
            // An opencode Run's recorded PID is only a daemon client, so it
            // says nothing about whether the work is done: the Run is finished
            // when its session has an outcome, or its log has a result.
            match self.opencode_run_result(worker, &loaded.value) {
                Some(result) => result,
                // Still running, or its session cannot be told apart from a
                // live one: never call the Worker finished on a guess.
                None => return Ok(()),
            }
        } else {
            let Some(pid) = loaded.value.pid else {
                return Ok(());
            };
            // The recorded PID is a liveness hint, so the Run counts as live
            // only when it still leads its process group. A recycled PID that
            // does not lead a group must not keep this Worker `busy` forever.
            let run_is_live = u32::try_from(pid)
                .ok()
                .and_then(crate::engine::runtime::live_process_group_leader)
                .is_some();
            if run_is_live {
                return Ok(());
            }
            run_log_finalization(&loaded.value, runtime).0
        };

        let revision = loaded.revision().clone();
        let mut state = loaded.value;
        apply_run_result(&mut state, &result)?;
        let record = run_record(&self.repository, worker, &state, &result);
        match repository.commit(Expected::Match(revision), StateChange::Replace(state)) {
            // This reconciliation is the one that observed the Run end, so it
            // owns the durable record - exactly as the spawning supervisor does
            // when its own waiter finalizes the Run first.
            Ok(CommitOutcome::Applied(_)) => {
                self.repository.state_store().runs().append(&record)?;
                Ok(())
            }
            // Another writer finalized this Run first (the supervisor's waiter,
            // or a concurrent reconciliation). That writer appended the record,
            // so appending here would count the Run twice.
            Ok(CommitOutcome::Conflict(_)) => Ok(()),
            Err(source) => Err(WorkerPoolError::Reconciliation {
                worker: worker.clone(),
                source,
            }),
        }
    }

    /// The result an opencode Run finished with, or `None` while its session is
    /// still running or cannot be consulted.
    ///
    /// The Run's work lives in the shared daemon, so the Run is finished only
    /// on positive evidence: the session reports an `outcome`, or - when the
    /// session cannot be read - the Run log has a provider terminal result.
    /// Anything else leaves the Worker `busy`, because a thin client that has
    /// exited says nothing about the work.
    fn opencode_run_result(&self, worker: &WorkerId, state: &WorkerState) -> Option<RunResult> {
        let (log_result, log_source) = run_log_finalization(state, Some(AgentRuntime::OpenCode));
        match self.opencode_session_state(worker, state) {
            // The session reports a terminal outcome. A provider terminal event
            // in the Run log carries more detail than the bare outcome, so
            // prefer it when the log has one. opencode reports cost and tokens
            // per step, so the Run's totals are summed from its own log.
            RunSessionState::Finished(outcome) => Some(opencode_totals(
                state,
                match log_source {
                    RunResultSource::Provider => log_result,
                    RunResultSource::Fallback { .. } => opencode_outcome_result(&outcome),
                },
            )),
            // No outcome yet, or the session cannot be read. Only a provider
            // terminal event in the Run log proves the Run ended: an *open*
            // session is not evidence of a *running* one - opencode reports a
            // session as active while it is merely open - so a log result still
            // finishes the Worker, and anything less leaves it `busy`.
            RunSessionState::Running | RunSessionState::Unavailable => {
                matches!(log_source, RunResultSource::Provider).then_some(log_result)
            }
        }
    }

    /// Consults the session the Worker's opencode Run drives.
    fn opencode_session_state(&self, worker: &WorkerId, state: &WorkerState) -> RunSessionState {
        let api = crate::engine::OpenCodeApi::new();
        let Some(session_id) = crate::engine::runtime::identify_run_session(
            &api,
            &self.repository,
            worker,
            state.ticket.as_deref(),
        ) else {
            return RunSessionState::Unavailable;
        };
        match api.get_session(&session_id) {
            Ok(session) => match session.outcome() {
                Some(outcome) => RunSessionState::Finished(outcome.clone()),
                None => RunSessionState::Running,
            },
            Err(_) => RunSessionState::Unavailable,
        }
    }

    /// Reads the Run a Reset would abandon, or `None` when the Worker is idle.
    pub(crate) fn run_target(
        &self,
        worker: &WorkerId,
    ) -> Result<Option<RunTarget>, WorkerPoolError> {
        let loaded = match self.worker_repository(worker).load()? {
            Loaded::Present(versioned) => versioned,
            Loaded::Missing => {
                return Err(WorkerPoolError::WorkerStateMissing {
                    worker: worker.clone(),
                });
            }
        };
        if loaded.value.status.as_str() == WorkerStatus::Idle.as_str() {
            return Ok(None);
        }
        let (state, revision) = loaded.into_parts();
        Ok(Some(RunTarget::new(&state, revision)))
    }

    /// Clears a terminal Worker Run without restoring its Workspace.
    ///
    /// This is the compatibility Dismiss path: completed or failed Worker
    /// state is released for reuse while the user keeps the Workspace contents
    /// available for inspection. Active Runs remain protected.
    pub fn clear_terminal(&self, worker: &WorkerId) -> Result<(), WorkerPoolError> {
        let snapshot = self.snapshot();
        let state =
            snapshot
                .worker(worker.as_str())
                .ok_or_else(|| WorkerPoolError::WorkerNotInPool {
                    worker: worker.clone(),
                })?;
        if state.status() == WorkerStatus::Busy {
            return Err(WorkerPoolError::WorkerNotIdle {
                worker: worker.clone(),
            });
        }
        let Some(target) = self.run_target(worker)? else {
            return Ok(());
        };
        match self.clear_run(worker, target)? {
            RunClearing::Cleared | RunClearing::AlreadyIdle => Ok(()),
            RunClearing::Superseded => Err(WorkerPoolError::Conflict),
        }
    }

    /// Returns a Worker to idle once its Run is no longer executing.
    ///
    /// A natural finalization of the same Run only changes completion fields,
    /// so the clearing retries against the fresh revision. A Worker that a
    /// different Run already owns is reported as superseded and left untouched.
    pub(crate) fn clear_run(
        &self,
        worker: &WorkerId,
        target: RunTarget,
    ) -> Result<RunClearing, WorkerPoolError> {
        let repository = self.worker_repository(worker);
        let mut expected = target.revision.clone();
        for _ in 0..CLEAR_RUN_ATTEMPTS {
            let loaded = match repository.load()? {
                Loaded::Present(versioned) => versioned,
                Loaded::Missing => {
                    return Err(WorkerPoolError::WorkerStateMissing {
                        worker: worker.clone(),
                    });
                }
            };
            if loaded.value.status.as_str() == WorkerStatus::Idle.as_str() {
                return Ok(RunClearing::AlreadyIdle);
            }
            if !target.is_same_run(&loaded.value) {
                return Ok(RunClearing::Superseded);
            }
            let mut state = loaded.value;
            clear_run_fields(&mut state);
            match repository.commit(Expected::Match(expected), StateChange::Replace(state))? {
                CommitOutcome::Applied(_) => return Ok(RunClearing::Cleared),
                CommitOutcome::Conflict(Loaded::Present(versioned)) => {
                    expected = versioned.revision().clone();
                }
                CommitOutcome::Conflict(Loaded::Missing) => {
                    return Err(WorkerPoolError::WorkerStateMissing {
                        worker: worker.clone(),
                    });
                }
            }
        }
        Err(WorkerPoolError::ResetConflict {
            worker: worker.clone(),
        })
    }

    fn worker_repository(&self, worker: &WorkerId) -> crate::engine::WorkerStateRepository {
        self.repository.state_store().worker(worker.clone())
    }

    pub(crate) fn begin_follow_up(
        &self,
        worker: &WorkerId,
        profile: AgentRuntimeProfile,
    ) -> Result<(Reservation, Option<PathBuf>), WorkerPoolError> {
        let started_at = current_timestamp()?;
        let log_path = self.repository.state_store().worker_log(worker);
        match self.repository.state_store().begin_follow_up(
            worker,
            started_at,
            log_path.to_string_lossy().into_owned(),
            Some(profile),
        )? {
            crate::engine::state::FollowUpOutcome::Started {
                profile,
                prior_log,
                revision,
                rollback,
            } => Ok((
                Reservation {
                    worker_id: worker.clone(),
                    ticket: rollback.ticket.clone().unwrap_or_default(),
                    profile,
                    repository: self.repository.clone(),
                    worker_revision: revision,
                    rollback: *rollback,
                },
                prior_log.map(PathBuf::from),
            )),
            crate::engine::state::FollowUpOutcome::WorkerNotInPool => {
                Err(WorkerPoolError::WorkerNotInPool {
                    worker: worker.clone(),
                })
            }
            crate::engine::state::FollowUpOutcome::WorkerBusy => {
                Err(WorkerPoolError::WorkerNotIdle {
                    worker: worker.clone(),
                })
            }
            crate::engine::state::FollowUpOutcome::WorkerStateMissing => {
                Err(WorkerPoolError::WorkerStateMissing {
                    worker: worker.clone(),
                })
            }
            crate::engine::state::FollowUpOutcome::InvalidAgentRuntime { message } => {
                Err(WorkerPoolError::InvalidAgentRuntime { message })
            }
        }
    }

    /// Reserves the first idle Worker for `ticket` in pool order.
    pub fn reserve(&self, ticket: impl Into<String>) -> Result<Reservation, WorkerPoolError> {
        self.reserve_inner(None, ticket.into(), None, &DispatchExclusions::default())
    }

    pub(crate) fn reserve_profiled(
        &self,
        requested: Option<WorkerId>,
        ticket: impl Into<String>,
        profile: AgentRuntimeProfile,
        excluded: &DispatchExclusions,
    ) -> Result<Reservation, WorkerPoolError> {
        self.reserve_inner(requested, ticket.into(), Some(profile), excluded)
    }

    /// Atomically reserves one idle Worker per Ticket in input order.
    ///
    /// The complete batch is all-or-nothing. When fewer idle Workers exist,
    /// no Worker state is changed.
    pub fn reserve_many<S: AsRef<str>>(
        &self,
        tickets: &[S],
    ) -> Result<Vec<Reservation>, WorkerPoolError> {
        let requests = tickets
            .iter()
            .map(|ticket| (ticket.as_ref().to_owned(), None, None))
            .collect::<Vec<_>>();
        self.reserve_targeted(&requests, &DispatchExclusions::default())
    }

    pub(crate) fn reserve_targeted(
        &self,
        requests: &[(String, Option<WorkerId>, Option<AgentRuntimeProfile>)],
        excluded: &DispatchExclusions,
    ) -> Result<Vec<Reservation>, WorkerPoolError> {
        let inputs = requests
            .iter()
            .map(|(ticket, worker, profile)| {
                Ok(crate::engine::state::ReservationInput {
                    branch_name: ticket.to_lowercase(),
                    ticket: ticket.clone(),
                    requested_worker: worker.clone(),
                    started_at: current_timestamp()?,
                    profile: profile.clone(),
                })
            })
            .collect::<Result<Vec<_>, WorkerPoolError>>()?;
        match self
            .repository
            .state_store()
            .reserve_workers(inputs, excluded)?
        {
            crate::engine::state::ReservationsOutcome::Reserved {
                workers,
                unavailable,
            } => {
                debug_assert!(unavailable.is_empty());
                Ok(workers
                    .into_iter()
                    .map(|reserved| Reservation {
                        worker_id: reserved.worker,
                        ticket: reserved.ticket,
                        profile: reserved.profile,
                        repository: self.repository.clone(),
                        worker_revision: reserved.revision,
                        rollback: reserved.rollback,
                    })
                    .collect())
            }
            crate::engine::state::ReservationsOutcome::NoIdle { available } => {
                Err(CapacityShortage::new(requests.len(), available).into())
            }
            crate::engine::state::ReservationsOutcome::WorkerNotInPool { worker } => {
                Err(WorkerPoolError::WorkerNotInPool { worker })
            }
            crate::engine::state::ReservationsOutcome::WorkerNotIdle { worker } => {
                Err(WorkerPoolError::WorkerNotIdle { worker })
            }
            crate::engine::state::ReservationsOutcome::WorkerNotEligible { worker } => {
                Err(WorkerPoolError::WorkerNotEligible { worker })
            }
            crate::engine::state::ReservationsOutcome::InvalidAgentRuntime { message } => {
                Err(WorkerPoolError::InvalidAgentRuntime { message })
            }
        }
    }

    pub(crate) fn reserve_available_targeted(
        &self,
        requests: &[(String, Option<WorkerId>, Option<AgentRuntimeProfile>)],
        excluded: &DispatchExclusions,
    ) -> Result<AvailableReservations, WorkerPoolError> {
        let inputs = requests
            .iter()
            .map(|(ticket, worker, profile)| {
                Ok(crate::engine::state::ReservationInput {
                    branch_name: ticket.to_lowercase(),
                    ticket: ticket.clone(),
                    requested_worker: worker.clone(),
                    started_at: current_timestamp()?,
                    profile: profile.clone(),
                })
            })
            .collect::<Result<Vec<_>, WorkerPoolError>>()?;
        match self
            .repository
            .state_store()
            .reserve_available_workers(inputs, excluded)?
        {
            crate::engine::state::ReservationsOutcome::Reserved {
                workers,
                unavailable,
            } => {
                let unavailable_set = unavailable.iter().copied().collect::<BTreeSet<_>>();
                let claimed = (0..requests.len())
                    .filter(|index| !unavailable_set.contains(index))
                    .zip(workers)
                    .map(|(index, reserved)| {
                        (
                            index,
                            Reservation {
                                worker_id: reserved.worker,
                                ticket: reserved.ticket,
                                profile: reserved.profile,
                                repository: self.repository.clone(),
                                worker_revision: reserved.revision,
                                rollback: reserved.rollback,
                            },
                        )
                    })
                    .collect();
                Ok(AvailableReservations {
                    claimed,
                    unavailable,
                })
            }
            crate::engine::state::ReservationsOutcome::InvalidAgentRuntime { message } => {
                Err(WorkerPoolError::InvalidAgentRuntime { message })
            }
            crate::engine::state::ReservationsOutcome::NoIdle { available } => {
                Err(CapacityShortage::new(requests.len(), available).into())
            }
            crate::engine::state::ReservationsOutcome::WorkerNotInPool { worker } => {
                Err(WorkerPoolError::WorkerNotInPool { worker })
            }
            crate::engine::state::ReservationsOutcome::WorkerNotIdle { worker } => {
                Err(WorkerPoolError::WorkerNotIdle { worker })
            }
            crate::engine::state::ReservationsOutcome::WorkerNotEligible { worker } => {
                Err(WorkerPoolError::WorkerNotEligible { worker })
            }
        }
    }

    /// Reserves a complete batch, provisioning at most the caller-approved gap.
    ///
    /// Provisioning runs outside state locks. Final Pool adoption and batch
    /// Reservation share one locked transition, so concurrent claimers cannot
    /// take newly added capacity between those steps.
    pub fn grow_and_reserve_many<S: AsRef<str>>(
        &self,
        tickets: &[S],
        approved_additional: usize,
    ) -> Result<Vec<Reservation>, WorkerPoolError> {
        let requests = tickets
            .iter()
            .map(|ticket| (ticket.as_ref().to_owned(), None, None))
            .collect::<Vec<_>>();
        self.grow_and_reserve_targeted(
            &requests,
            approved_additional,
            &DispatchExclusions::default(),
        )
    }

    pub(crate) fn grow_and_reserve_targeted(
        &self,
        requests: &[(String, Option<WorkerId>, Option<AgentRuntimeProfile>)],
        approved_additional: usize,
        excluded: &DispatchExclusions,
    ) -> Result<Vec<Reservation>, WorkerPoolError> {
        let shortage = match self.reserve_targeted(requests, excluded) {
            Ok(reservations) => return Ok(reservations),
            Err(WorkerPoolError::CapacityShortage(shortage)) => shortage,
            Err(error) => return Err(error),
        };
        if shortage.gap() > approved_additional {
            return Err(shortage.into());
        }

        let pool_repository = self.repository.state_store().pool();
        let current = self.load_or_create(&pool_repository)?;
        let current_size = usize::try_from(current.value.size)
            .map_err(|_| WorkerPoolError::InvalidSize(current.value.size))?;
        let mut known = current
            .value
            .workers
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut added = Vec::with_capacity(shortage.gap());
        for _ in 0..shortage.gap() {
            let worker = next_worker_id(&self.repository, &known)?;
            match self.repository.provision_worker_workspace(&worker) {
                Ok(_) => {
                    known.insert(worker.clone());
                    added.push(worker);
                }
                Err(source) => {
                    return match self.cleanup_workers(&added) {
                        Ok(()) => Err(WorkerPoolError::Provision { worker, source }),
                        Err(cleanup) => Err(WorkerPoolError::Compensation(format!(
                            "{source}; {cleanup}"
                        ))),
                    };
                }
            }
        }

        let mut next = current.value.clone();
        next.size = i64::try_from(current_size + added.len())
            .map_err(|_| WorkerPoolError::InvalidSize(next.size))?;
        next.workers.extend(added.iter().cloned());
        let inputs = requests
            .iter()
            .map(|(ticket, worker, profile)| {
                Ok(crate::engine::state::ReservationInput {
                    branch_name: ticket.to_lowercase(),
                    ticket: ticket.clone(),
                    requested_worker: worker.clone(),
                    started_at: current_timestamp()?,
                    profile: profile.clone(),
                })
            })
            .collect::<Result<Vec<_>, WorkerPoolError>>()?;
        let outcome = self.repository.state_store().grow_and_reserve_workers(
            current.revision().clone(),
            next,
            inputs,
            excluded,
        );
        let result = match outcome {
            Ok(crate::engine::state::GrowReservationsOutcome::Reserved(reserved)) => Ok(reserved
                .into_iter()
                .map(|reserved| Reservation {
                    worker_id: reserved.worker,
                    ticket: reserved.ticket,
                    profile: reserved.profile,
                    repository: self.repository.clone(),
                    worker_revision: reserved.revision,
                    rollback: reserved.rollback,
                })
                .collect()),
            Ok(crate::engine::state::GrowReservationsOutcome::Conflict) => {
                Err(WorkerPoolError::Conflict)
            }
            Ok(crate::engine::state::GrowReservationsOutcome::NoIdle { available }) => {
                Err(CapacityShortage::new(requests.len(), available).into())
            }
            Ok(crate::engine::state::GrowReservationsOutcome::InvalidAgentRuntime { message }) => {
                Err(WorkerPoolError::InvalidAgentRuntime { message })
            }
            Err(error) => Err(WorkerPoolError::State(error)),
        };
        if result.is_err()
            && let Err(cleanup) = self.cleanup_workers(&added)
        {
            return Err(WorkerPoolError::Compensation(format!(
                "{result:?}; {cleanup}"
            )));
        }
        result
    }

    /// Reserves the named idle Worker for `ticket`.
    pub fn reserve_named(
        &self,
        worker: WorkerId,
        ticket: impl Into<String>,
    ) -> Result<Reservation, WorkerPoolError> {
        self.reserve_inner(
            Some(worker),
            ticket.into(),
            None,
            &DispatchExclusions::default(),
        )
    }

    /// Replaces the Repository default profile used by future Reservations.
    ///
    /// The profile is stored in the Pool document, so a Repository with no Pool
    /// state yet would otherwise fail before it could name its runtime - and the
    /// documented dispatch order names the runtime before capacity. Naming a
    /// runtime therefore provisions the empty Pool document first, exactly as
    /// `jjfx pool <N>` does, so either order works. It provisions no capacity:
    /// `jjfx pool <N>` remains the only thing that creates Workers.
    pub fn set_profile(&self, profile: AgentRuntimeProfile) -> Result<(), WorkerPoolError> {
        self.load_or_create(&self.repository.state_store().pool())?;
        self.repository.state_store().set_pool_profile(&profile)?;
        Ok(())
    }

    /// Sets a Worker's cosmetic display alias, or clears it when `text` is blank.
    pub fn set_alias(
        &self,
        worker: WorkerId,
        text: impl Into<String>,
    ) -> Result<(), WorkerPoolError> {
        let text = text.into();
        let alias = match text.trim() {
            "" => None,
            normalized => Some(normalized.to_owned()),
        };
        match self
            .repository
            .state_store()
            .set_worker_alias(&worker, alias)?
        {
            crate::engine::state::PoolAliasOutcome::Updated => Ok(()),
            crate::engine::state::PoolAliasOutcome::WorkerNotInPool => {
                Err(WorkerPoolError::WorkerNotInPool { worker })
            }
            crate::engine::state::PoolAliasOutcome::Destroying => Err(WorkerPoolError::Conflict),
        }
    }

    fn reserve_inner(
        &self,
        requested: Option<WorkerId>,
        ticket: String,
        profile: Option<AgentRuntimeProfile>,
        excluded: &DispatchExclusions,
    ) -> Result<Reservation, WorkerPoolError> {
        let started_at = current_timestamp()?;
        let outcome = self.repository.state_store().reserve_worker(
            requested.as_ref(),
            ticket.clone(),
            started_at,
            ticket.to_lowercase(),
            profile,
            excluded,
        )?;
        match outcome {
            crate::engine::state::ReservationOutcome::Reserved {
                worker,
                profile,
                revision,
                rollback,
            } => Ok(Reservation {
                worker_id: worker,
                ticket,
                profile,
                repository: self.repository.clone(),
                worker_revision: revision,
                rollback: *rollback,
            }),
            crate::engine::state::ReservationOutcome::NoIdle { available } => {
                Err(WorkerPoolError::NoIdleWorkers { ticket, available })
            }
            crate::engine::state::ReservationOutcome::WorkerNotInPool { worker } => {
                Err(WorkerPoolError::WorkerNotInPool { worker })
            }
            crate::engine::state::ReservationOutcome::WorkerNotIdle { worker } => {
                Err(WorkerPoolError::WorkerNotIdle { worker })
            }
            crate::engine::state::ReservationOutcome::WorkerNotEligible { worker } => {
                Err(WorkerPoolError::WorkerNotEligible { worker })
            }
            crate::engine::state::ReservationOutcome::InvalidAgentRuntime { message } => {
                Err(WorkerPoolError::InvalidAgentRuntime { message })
            }
        }
    }

    /// Materialises a declared Pool's state on first use.
    ///
    /// A declared Pool with no state directory is normal, not a disagreement
    /// (ADR 0013): `[pools.<name>]` is the source of truth and
    /// `.jj/pools/<name>/` is state, so the first operation that needs the Worker
    /// Pool's document creates it - provisioning the Workers the declaration names,
    /// exactly as `jjfx pool <N>` does for `default`. That is what makes a
    /// committed declaration work on a fresh clone with nobody provisioning the
    /// lane by hand.
    ///
    /// Idempotent: a Pool whose state directory already exists is left untouched,
    /// so a second Dispatch never re-provisions it. Provisioning runs outside
    /// state locks and adoption takes the Worker Pool lock, so this is safe under
    /// the repository-outermost lock order (ADR 0013).
    pub fn materialise(&self, capacity: PoolCapacity) -> Result<Vec<WorkerId>, WorkerPoolError> {
        if self.repository.state_store().directory_exists() {
            return Ok(Vec::new());
        }
        Ok(self.resize_to(capacity)?.added_workers().to_vec())
    }

    /// Resizes the Worker Pool, provisioning new Workers or detaching its stable tail.
    pub fn resize_to(&self, capacity: PoolCapacity) -> Result<PoolResize, WorkerPoolError> {
        self.resize_to_with_progress(capacity, |_, _| {})
    }

    /// Resizes the Worker Pool while reporting lifecycle hook output.
    pub fn resize_to_with_progress<F>(
        &self,
        capacity: PoolCapacity,
        mut on_output: F,
    ) -> Result<PoolResize, WorkerPoolError>
    where
        F: FnMut(crate::engine::workspace::WorkspaceHookStream, &str),
    {
        let mut recovered = self.retry_detached_cleanup(&mut on_output)?;
        let pool_repository = self.repository.state_store().pool();
        let current = self.load_or_create(&pool_repository)?;
        let current_size = usize::try_from(current.value.size)
            .map_err(|_| WorkerPoolError::InvalidSize(current.value.size))?;
        if capacity.as_usize() >= current_size {
            let mut growth = self.grow(capacity, &mut on_output)?;
            growth.removed_workers = recovered;
            return Ok(growth);
        }

        match self
            .repository
            .state_store()
            .shrink_pool(capacity.as_usize())?
        {
            crate::engine::state::PoolMembershipOutcome::Changed { capacity, removed } => {
                recovered.extend(self.cleanup_detached_with_progress(&removed, &mut on_output)?);
                Ok(PoolResize {
                    capacity: PoolCapacity::new(capacity)
                        .map_err(|_| WorkerPoolError::InvalidSize(capacity as i64))?,
                    added_workers: Vec::new(),
                    removed_workers: recovered,
                })
            }
            crate::engine::state::PoolMembershipOutcome::NoChange { capacity } => Ok(PoolResize {
                capacity: PoolCapacity::new(capacity)
                    .map_err(|_| WorkerPoolError::InvalidSize(capacity as i64))?,
                added_workers: Vec::new(),
                removed_workers: recovered,
            }),
            crate::engine::state::PoolMembershipOutcome::NeedsGrowth => {
                Err(WorkerPoolError::Conflict)
            }
            crate::engine::state::PoolMembershipOutcome::Busy { workers } => {
                Err(WorkerPoolError::WorkersBusy { workers })
            }
            crate::engine::state::PoolMembershipOutcome::WorkerNotInPool { .. } => {
                Err(WorkerPoolError::Conflict)
            }
        }
    }

    /// Removes one named non-busy Worker while preserving remaining Pool order.
    pub fn remove(&self, worker: WorkerId) -> Result<PoolResize, WorkerPoolError> {
        self.remove_with_progress(worker, |_, _| {})
    }

    /// Removes a Worker while reporting teardown hook output.
    pub fn remove_with_progress<F>(
        &self,
        worker: WorkerId,
        mut on_output: F,
    ) -> Result<PoolResize, WorkerPoolError>
    where
        F: FnMut(crate::engine::workspace::WorkspaceHookStream, &str),
    {
        match self.repository.state_store().remove_pool_worker(&worker)? {
            crate::engine::state::PoolMembershipOutcome::Changed { capacity, removed } => {
                let removed = self.cleanup_detached_with_progress(&removed, &mut on_output)?;
                Ok(PoolResize {
                    capacity: PoolCapacity::new(capacity)
                        .map_err(|_| WorkerPoolError::InvalidSize(capacity as i64))?,
                    added_workers: Vec::new(),
                    removed_workers: removed,
                })
            }
            crate::engine::state::PoolMembershipOutcome::Busy { workers } => {
                Err(WorkerPoolError::WorkersBusy { workers })
            }
            crate::engine::state::PoolMembershipOutcome::WorkerNotInPool { worker } => {
                if !self.worker_repository(&worker).cleanup_marker_exists() {
                    return Err(WorkerPoolError::WorkerNotInPool { worker });
                }
                let removed = self.cleanup_detached_with_progress(
                    std::slice::from_ref(&worker),
                    &mut on_output,
                )?;
                if removed.is_empty() {
                    return Err(WorkerPoolError::WorkerNotInPool { worker });
                }
                let capacity = self.current_capacity()?;
                Ok(PoolResize {
                    capacity,
                    added_workers: Vec::new(),
                    removed_workers: removed,
                })
            }
            crate::engine::state::PoolMembershipOutcome::NoChange { .. }
            | crate::engine::state::PoolMembershipOutcome::NeedsGrowth => {
                Err(WorkerPoolError::Conflict)
            }
        }
    }

    /// Destroys the Worker Pool, terminating recorded Runs and removing all its
    /// resources.
    ///
    /// Membership is detached before any process or Workspace operation runs.
    /// Worker state and cleanup markers remain durable until external cleanup
    /// succeeds, so repeating this operation resumes an interrupted destroy.
    ///
    /// This is the forced path: it terminates whatever is running rather than
    /// refusing. `pool destroy` drains first and only reaches here with
    /// `--force` when a Worker is busy.
    pub fn destroy(&self) -> Result<(), WorkerPoolError> {
        self.destroy_with_progress(|_, _| {})
    }

    /// Destroys the Worker Pool while reporting teardown hook output.
    pub fn destroy_with_progress<F>(&self, mut on_output: F) -> Result<(), WorkerPoolError>
    where
        F: FnMut(crate::engine::workspace::WorkspaceHookStream, &str),
    {
        destroy_pool_state(
            &self.repository,
            &self.repository.state_store(),
            &mut on_output,
        )
    }

    fn current_capacity(&self) -> Result<PoolCapacity, WorkerPoolError> {
        let loaded = self.repository.state_store().pool().load()?;
        let Loaded::Present(pool) = loaded else {
            return Err(WorkerPoolError::Conflict);
        };
        let size = usize::try_from(pool.value.size)
            .map_err(|_| WorkerPoolError::InvalidSize(pool.value.size))?;
        PoolCapacity::new(size).map_err(|_| WorkerPoolError::InvalidSize(pool.value.size))
    }

    /// Grows the pool to `capacity`, provisioning stable Worker identities.
    ///
    /// A missing pool is first initialized with compatible metadata. Workspace
    /// commands run outside state locks; the final manifest update uses the
    /// loaded exact-byte revision, and newly provisioned Workers are
    /// compensated if another process wins the mutation.
    fn grow<F>(
        &self,
        capacity: PoolCapacity,
        on_output: &mut F,
    ) -> Result<PoolResize, WorkerPoolError>
    where
        F: FnMut(crate::engine::workspace::WorkspaceHookStream, &str),
    {
        let pool = self.repository.state_store().pool();
        let current = self.load_or_create(&pool)?;
        let current_size = usize::try_from(current.value.size)
            .map_err(|_| WorkerPoolError::InvalidSize(current.value.size))?;
        if capacity.as_usize() < current_size {
            return Err(WorkerPoolError::Conflict);
        }
        if capacity.as_usize() == current_size {
            return Ok(PoolResize {
                capacity,
                added_workers: Vec::new(),
                removed_workers: Vec::new(),
            });
        }

        let mut known = current
            .value
            .workers
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let count = capacity.as_usize() - current_size;
        let mut added = Vec::with_capacity(count);
        for _ in 0..count {
            let worker = next_worker_id(&self.repository, &known)?;
            match self
                .repository
                .provision_worker_workspace_with_progress(&worker, &mut *on_output)
            {
                Ok(_) => {
                    known.insert(worker.clone());
                    added.push(worker);
                }
                Err(source) => {
                    return match self.cleanup_workers_with_progress(&added, on_output) {
                        Ok(()) => Err(WorkerPoolError::Provision { worker, source }),
                        Err(cleanup) => Err(WorkerPoolError::Compensation(format!(
                            "{source}; {cleanup}"
                        ))),
                    };
                }
            }
        }

        let mut next = current.value.clone();
        next.size = i64::try_from(capacity.as_usize())
            .map_err(|_| WorkerPoolError::InvalidSize(next.size))?;
        next.workers.extend(added.iter().cloned());
        let committed = self
            .repository
            .state_store()
            .commit_pool_growth(current.revision().clone(), next);
        match committed {
            Ok(CommitOutcome::Applied(_)) => Ok(PoolResize {
                capacity,
                added_workers: added,
                removed_workers: Vec::new(),
            }),
            Ok(CommitOutcome::Conflict(_)) => {
                match self.cleanup_workers_with_progress(&added, on_output) {
                    Ok(()) => Err(WorkerPoolError::Conflict),
                    Err(cleanup) => Err(WorkerPoolError::Compensation(cleanup.to_string())),
                }
            }
            Err(error) => match self.cleanup_workers_with_progress(&added, on_output) {
                Ok(()) => Err(WorkerPoolError::State(error)),
                Err(cleanup) => Err(WorkerPoolError::Compensation(format!("{error}; {cleanup}"))),
            },
        }
    }

    fn retry_detached_cleanup<F>(&self, on_output: &mut F) -> Result<Vec<WorkerId>, WorkerPoolError>
    where
        F: FnMut(crate::engine::workspace::WorkspaceHookStream, &str),
    {
        let markers = self.repository.state_store().detached_cleanup_markers()?;
        self.cleanup_detached_with_progress(&markers, on_output)
    }

    fn cleanup_detached_with_progress<F>(
        &self,
        workers: &[WorkerId],
        on_output: &mut F,
    ) -> Result<Vec<WorkerId>, WorkerPoolError>
    where
        F: FnMut(crate::engine::workspace::WorkspaceHookStream, &str),
    {
        let state_store = self.repository.state_store();
        let mut cleaned = Vec::new();
        let mut busy = Vec::new();
        let mut failures = Vec::new();
        for worker in workers {
            match state_store.detached_cleanup_status(worker) {
                Ok(crate::engine::state::DetachedCleanupStatus::Ready) => {
                    if let Err(error) = crate::engine::workspace::teardown_detached_with_progress(
                        &self.repository,
                        &state_store,
                        worker,
                        &mut *on_output,
                    ) {
                        failures.push(format!("{worker}: {error}"));
                        continue;
                    }
                    match state_store.finish_detached_cleanup(worker) {
                        Ok(true) => cleaned.push(worker.clone()),
                        Ok(false) => failures.push(format!(
                            "{worker}: Worker rejoined the Worker Pool during cleanup"
                        )),
                        Err(error) => failures.push(format!("{worker}: {error}")),
                    }
                }
                Ok(crate::engine::state::DetachedCleanupStatus::Busy { .. }) => {
                    busy.push(worker.clone())
                }
                Ok(crate::engine::state::DetachedCleanupStatus::NotDetached) => {}
                Err(error) => failures.push(format!("{worker}: {error}")),
            }
        }
        if !busy.is_empty() {
            return Err(WorkerPoolError::WorkersBusy { workers: busy });
        }
        if failures.is_empty() {
            Ok(cleaned)
        } else {
            Err(WorkerPoolError::Cleanup(failures.join("; ")))
        }
    }

    fn cleanup_workers(&self, workers: &[WorkerId]) -> Result<(), WorkerPoolError> {
        let mut failures = Vec::new();
        for worker in workers {
            if let Err(error) = crate::engine::workspace::deprovision(&self.repository, worker) {
                failures.push(format!("{worker}: {error}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(WorkerPoolError::Compensation(failures.join("; ")))
        }
    }

    fn cleanup_workers_with_progress<F>(
        &self,
        workers: &[WorkerId],
        on_output: &mut F,
    ) -> Result<(), WorkerPoolError>
    where
        F: FnMut(crate::engine::workspace::WorkspaceHookStream, &str),
    {
        let mut failures = Vec::new();
        for worker in workers {
            if let Err(error) = crate::engine::workspace::deprovision_with_progress(
                &self.repository,
                worker,
                &mut *on_output,
            ) {
                failures.push(format!("{worker}: {error}"));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(WorkerPoolError::Compensation(failures.join("; ")))
        }
    }

    fn load_or_create(
        &self,
        pool: &crate::engine::PoolStateRepository,
    ) -> Result<crate::engine::Versioned<PoolState>, WorkerPoolError> {
        match pool.load()? {
            Loaded::Present(versioned) => Ok(versioned),
            Loaded::Missing => {
                let empty = PoolState::new(
                    0,
                    discover_gh_repo(&self.repository)?,
                    Vec::new(),
                    current_timestamp()?,
                );
                match pool.commit(Expected::Missing, StateChange::Replace(empty))? {
                    CommitOutcome::Applied(Loaded::Present(versioned))
                    | CommitOutcome::Conflict(Loaded::Present(versioned)) => Ok(versioned),
                    CommitOutcome::Applied(Loaded::Missing)
                    | CommitOutcome::Conflict(Loaded::Missing) => Err(WorkerPoolError::Conflict),
                }
            }
        }
    }
}

/// Destroys one Pool's state and every Worker Workspace it owns.
///
/// The Pool's state store is the caller's, so this destroys `default` or a
/// named Pool by the same mechanism. Membership is detached before any process
/// or Workspace operation runs, and Worker state stays durable until external
/// cleanup succeeds, so repeating it resumes an interrupted destroy.
fn destroy_pool_state<F>(
    repository: &Repository,
    state_store: &crate::engine::StateStore,
    on_output: &mut F,
) -> Result<(), WorkerPoolError>
where
    F: FnMut(crate::engine::workspace::WorkspaceHookStream, &str),
{
    state_store.detach_pool_for_destroy()?;
    let workers = state_store.detached_cleanup_markers()?;
    destroy_detached(repository, state_store, &workers, on_output)?;
    state_store.finish_pool_destroy()?;
    Ok(())
}

fn destroy_detached<F>(
    repository: &Repository,
    state_store: &crate::engine::StateStore,
    workers: &[WorkerId],
    on_output: &mut F,
) -> Result<(), WorkerPoolError>
where
    F: FnMut(crate::engine::workspace::WorkspaceHookStream, &str),
{
    let mut residual = Vec::new();
    let mut failures = Vec::new();
    for worker in workers {
        let status = match state_store.detached_cleanup_status(worker) {
            Ok(status) => status,
            Err(error) => {
                residual.push(worker.clone());
                failures.push(format!("{worker}: {error}"));
                continue;
            }
        };
        match status {
            crate::engine::state::DetachedCleanupStatus::Busy {
                pid: Some(pid),
                runtime,
            } => {
                if let Err(error) = crate::engine::RunSupervisor::new()
                    .terminate_recorded_process(repository, worker, runtime, pid)
                {
                    residual.push(worker.clone());
                    failures.push(format!(
                        "{worker}: cannot terminate process group {pid}: {error}"
                    ));
                    continue;
                }
            }
            crate::engine::state::DetachedCleanupStatus::Busy { pid: None, .. }
            | crate::engine::state::DetachedCleanupStatus::Ready => {}
            crate::engine::state::DetachedCleanupStatus::NotDetached => continue,
        }
        if let Err(error) = crate::engine::workspace::teardown_detached_with_progress(
            repository,
            state_store,
            worker,
            &mut *on_output,
        ) {
            residual.push(worker.clone());
            failures.push(format!("{worker}: {error}"));
            continue;
        }
        match state_store.finish_detached_cleanup(worker) {
            Ok(true) => {}
            Ok(false) => {
                residual.push(worker.clone());
                failures.push(format!(
                    "{worker}: Worker rejoined the Worker Pool during destruction"
                ));
            }
            Err(error) => {
                residual.push(worker.clone());
                failures.push(format!("{worker}: {error}"));
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(WorkerPoolError::DestroyCleanup {
            workers: residual,
            details: failures.join("; "),
        })
    }
}

fn next_worker_id(
    repository: &Repository,
    known: &BTreeSet<WorkerId>,
) -> Result<WorkerId, WorkerPoolError> {
    for _ in 0..32 {
        let mut bytes = [0_u8; 3];
        getrandom::fill(&mut bytes)
            .map_err(|error| WorkerPoolError::WorkerId(error.to_string()))?;
        let candidate = WorkerId::parse(format!(
            "worker-{:02x}{:02x}{:02x}",
            bytes[0], bytes[1], bytes[2]
        ))
        .map_err(|error| WorkerPoolError::WorkerId(error.to_string()))?;
        if !known.contains(&candidate) && !worker_claimed(repository, &candidate) {
            return Ok(candidate);
        }
    }
    Err(WorkerPoolError::WorkerId(
        "could not find an unused identifier after 32 attempts".to_owned(),
    ))
}

fn worker_claimed(repository: &Repository, worker: &WorkerId) -> bool {
    let state = repository.state_store().worker(worker.clone());
    crate::engine::workspace::worker_path(repository.root(), worker).exists()
        || state.cleanup_marker_exists()
        || state
            .load()
            .is_ok_and(|state| matches!(state, Loaded::Present(_)))
}

fn discover_gh_repo(repository: &Repository) -> Result<String, WorkerPoolError> {
    let output = crate::engine::workspace::git_remotes(repository.root())
        .map_err(|error| WorkerPoolError::RepositoryDiscovery(error.to_string()))?;
    if !output.status.success() {
        return Err(WorkerPoolError::RepositoryDiscovery(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() >= 2 && fields[0] == "origin" {
            return Ok(crate::engine::workspace::remote_slug(fields[1]));
        }
    }
    Ok(String::new())
}

/// Clears every Run-owned Worker field, leaving alias metadata and unknown
/// persisted extensions to the caller's loaded document.
fn clear_run_fields(state: &mut WorkerState) {
    state.status = WireStatus::new(WorkerStatus::Idle.as_str());
    state.agent = None;
    state.provider = None;
    state.model = None;
    state.ticket = None;
    state.pid = None;
    state.started_at = None;
    state.completed_at = None;
    state.log_file = None;
    state.branch_name = None;
    state.exit_code = None;
    state.error = None;
}

/// What a Worker's opencode session says about its Run.
enum RunSessionState {
    /// The session has no outcome yet, so its Run is still executing.
    Running,
    /// The session finished with this outcome.
    Finished(SessionOutcome),
    /// The session could not be identified or read.
    Unavailable,
}

/// Whether a Worker's persisted Agent Runtime is one this build removed.
fn is_removed_worker_runtime(state: &WorkerState) -> bool {
    state
        .agent
        .as_ref()
        .is_some_and(|agent| crate::engine::runtime::is_removed_runtime(agent.as_str()))
}

/// The clear removal message for a Worker whose persisted Agent Runtime was
/// removed, or `None` when its runtime is one this build still supports.
fn removed_worker_runtime(state: &WorkerState) -> Option<String> {
    state
        .agent
        .as_ref()
        .map(crate::engine::WireAgent::as_str)
        .filter(|agent| crate::engine::runtime::is_removed_runtime(agent))
        .map(crate::engine::runtime::removed_runtime_message)
}

/// The terminal result a Run log provides, or the compatible unexpected-exit
/// fallback when the log cannot provide one.
fn run_log_finalization(
    state: &WorkerState,
    runtime: Option<AgentRuntime>,
) -> (RunResult, RunResultSource) {
    runtime.zip(state.log_file.as_deref()).map_or_else(
        || {
            crate::engine::run_log::unexpected_result(
                crate::engine::RunResultFallback::MissingTerminalEvent,
            )
        },
        |(runtime, path)| crate::engine::run_log::result_for_finalization(Path::new(path), runtime),
    )
}

/// The result implied by a session `outcome`, used when the Run log carries no
/// provider terminal event of its own.
fn opencode_outcome_result(outcome: &SessionOutcome) -> RunResult {
    match outcome {
        SessionOutcome::Succeeded => RunResult::succeeded(),
        SessionOutcome::Failed => RunResult::failed("OpenCode session failed"),
        SessionOutcome::Interrupted => RunResult::failed("OpenCode session interrupted"),
        SessionOutcome::Unknown(outcome) => {
            RunResult::failed(format!("OpenCode session ended: {outcome}"))
        }
    }
}

/// Adds the cost and tokens opencode reports in its Run log to `result`.
///
/// The Run is already terminal; this only fills in what the provider's own step
/// events say it spent, so a Run whose log reports neither is unchanged.
fn opencode_totals(state: &WorkerState, result: RunResult) -> RunResult {
    match state.log_file.as_deref() {
        Some(path) => crate::engine::run_log::with_opencode_totals(result, Path::new(path)),
        None => result,
    }
}

fn apply_run_result(state: &mut WorkerState, result: &RunResult) -> Result<(), WorkerPoolError> {
    state.completed_at = Some(current_timestamp()?);
    match result.conclusion() {
        RunConclusion::Succeeded => {
            state.status = WireStatus::new("done");
            state.exit_code = Some(0);
            state.error = None;
        }
        RunConclusion::Failed { message } => {
            state.status = WireStatus::new("failed");
            state.exit_code = Some(1);
            state.error = Some(message.clone());
        }
    }
    Ok(())
}

/// Builds the durable record for a Run that has just been finalized.
///
/// Everything here is already on the finalized Worker state or the Run result,
/// so nothing new is parsed. An absent cost, usage, duration or turns stays
/// absent: a Run whose cost could not be read is a Run with no reported cost,
/// never a Run that fails to record.
fn run_record(
    repository: &Repository,
    worker: &WorkerId,
    state: &WorkerState,
    result: &RunResult,
) -> RunRecord {
    let workspace = crate::engine::workspace::worker_path(repository.root(), worker);
    let (conclusion, error) = match result.conclusion() {
        RunConclusion::Succeeded => (RunRecordConclusion::Succeeded, None),
        RunConclusion::Failed { message } => (RunRecordConclusion::Failed, Some(message.clone())),
    };
    RunRecord {
        run_id: run_identity(worker, state),
        worker: worker.clone(),
        ticket: state.ticket.clone(),
        workspace: Some(workspace.to_string_lossy().into_owned()),
        branch: state.branch_name.clone(),
        runtime: state.agent.as_ref().map(|agent| agent.as_str().to_owned()),
        provider: state.provider.clone(),
        model: state.model.clone(),
        cost_micro_usd: result.cost().map(RunCost::as_micro_usd),
        usage: result.usage().map(|usage| RunRecordUsage {
            input_tokens: usage.input_tokens(),
            cached_input_tokens: usage.cached_input_tokens(),
            cache_write_input_tokens: usage.cache_write_input_tokens(),
            output_tokens: usage.output_tokens(),
            reasoning_output_tokens: usage.reasoning_output_tokens(),
        }),
        duration_ms: result
            .duration()
            .and_then(|duration| u64::try_from(duration.as_millis()).ok()),
        turns: result.turns(),
        conclusion,
        error,
        started_at: state.started_at.clone(),
        completed_at: state.completed_at.clone(),
    }
}

/// The identity of a Run: its Worker and the timestamp its Reservation recorded.
///
/// These are the same Run-owned values a Reset compares to tell one Run from the
/// next, so a record's identity matches the Run a reader can see in Worker state.
fn run_identity(worker: &WorkerId, state: &WorkerState) -> String {
    match state.started_at.as_ref() {
        Some(started_at) => format!("{worker}@{}", started_at.as_str()),
        None => worker.as_str().to_owned(),
    }
}

pub(crate) fn current_timestamp() -> Result<WireTimestamp, WorkerPoolError> {
    let timestamp = Timestamp::now()
        .round(
            TimestampRound::new()
                .smallest(Unit::Second)
                .mode(RoundMode::Trunc),
        )
        .map_err(|error| WorkerPoolError::Timestamp(error.to_string()))?;
    Ok(WireTimestamp::new(timestamp.to_string()))
}

/// The execution-capacity state recorded for a Worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerStatus {
    Idle,
    Busy,
    Done,
    Failed,
}
impl WorkerStatus {
    fn parse(value: &WireStatus) -> Option<Self> {
        match value.as_str() {
            "idle" => Some(Self::Idle),
            "busy" => Some(Self::Busy),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Busy => "busy",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

/// Presence of an optional field in a persisted compatibility document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistedField<T> {
    Missing,
    Null,
    Value(T),
}

/// A Worker reference from the pool manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerReference {
    worker_id: WorkerId,
    workspace: String,
}
impl WorkerReference {
    pub fn worker_id(&self) -> &WorkerId {
        &self.worker_id
    }
    pub fn workspace(&self) -> &str {
        &self.workspace
    }
}

/// The successfully parsed pool manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolSnapshot {
    size: i64,
    gh_repo: String,
    foreground: Option<bool>,
    profile: Option<AgentRuntimeProfile>,
    workers: Vec<WorkerReference>,
}
impl PoolSnapshot {
    /// Compatibility schema generation. Go pool files have no explicit version.
    pub fn version(&self) -> u8 {
        1
    }
    pub fn size(&self) -> i64 {
        self.size
    }
    pub fn gh_repo(&self) -> &str {
        &self.gh_repo
    }
    /// Returns the configured foreground default, when explicitly persisted.
    pub fn foreground(&self) -> Option<bool> {
        self.foreground
    }
    /// Returns the configured Agent Runtime, when explicitly persisted.
    pub fn agent_runtime(&self) -> Option<AgentRuntime> {
        self.profile.as_ref().map(AgentRuntimeProfile::runtime)
    }
    /// Returns the configured default runtime and model profile.
    pub fn profile(&self) -> Option<&AgentRuntimeProfile> {
        self.profile.as_ref()
    }
    pub fn workers(&self) -> &[WorkerReference] {
        &self.workers
    }
}

/// A successfully parsed Worker document joined to its pool membership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerSnapshot {
    worker_id: WorkerId,
    alias: String,
    workspace: String,
    status: WorkerStatus,
    ticket: Option<String>,
    profile: Option<AgentRuntimeProfile>,
    started_at: Option<String>,
    completed_at: Option<String>,
    log_file: Option<String>,
    branch_name: Option<String>,
    error: Option<String>,
    exit_code: Option<i64>,
    pid: Option<u32>,
    process_alive: Option<bool>,
    /// When the Run log was last written, when it could be read. The watchdog's
    /// cheap progress signal, captured once per snapshot so a frontend need not
    /// stat the log per frame.
    log_activity_at: Option<Timestamp>,
}
impl WorkerSnapshot {
    pub fn version(&self) -> u8 {
        1
    }
    pub fn worker_id(&self) -> &WorkerId {
        &self.worker_id
    }
    pub fn alias(&self) -> &str {
        &self.alias
    }
    pub fn workspace(&self) -> &str {
        &self.workspace
    }
    pub fn status(&self) -> WorkerStatus {
        self.status
    }
    pub fn ticket(&self) -> Option<&str> {
        self.ticket.as_deref()
    }
    pub fn ticket_presence(&self) -> PersistedField<&str> {
        self.ticket
            .as_deref()
            .map_or(PersistedField::Null, PersistedField::Value)
    }
    /// Go Worker state has no separate Run identifier.
    pub fn run(&self) -> Option<&str> {
        None
    }
    pub fn run_presence(&self) -> PersistedField<&str> {
        PersistedField::Missing
    }
    pub fn agent_runtime(&self) -> Option<AgentRuntime> {
        self.profile.as_ref().map(AgentRuntimeProfile::runtime)
    }
    /// Returns the current or latest Run's complete runtime profile.
    pub fn profile(&self) -> Option<&AgentRuntimeProfile> {
        self.profile.as_ref()
    }
    pub fn agent_runtime_presence(&self) -> PersistedField<AgentRuntime> {
        self.agent_runtime()
            .map_or(PersistedField::Null, PersistedField::Value)
    }
    pub fn started_at(&self) -> Option<&str> {
        self.started_at.as_deref()
    }
    pub fn started_at_presence(&self) -> PersistedField<&str> {
        self.started_at
            .as_deref()
            .map_or(PersistedField::Null, PersistedField::Value)
    }
    /// Uses Go's completion time as the latest persisted activity timestamp.
    pub fn last_activity_at(&self) -> Option<&str> {
        self.completed_at.as_deref().or(self.started_at.as_deref())
    }
    pub fn last_activity_at_presence(&self) -> PersistedField<&str> {
        self.last_activity_at()
            .map_or(PersistedField::Null, PersistedField::Value)
    }
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub fn error_presence(&self) -> PersistedField<&str> {
        self.error
            .as_deref()
            .map_or(PersistedField::Null, PersistedField::Value)
    }
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }
    pub fn completed_at(&self) -> Option<&str> {
        self.completed_at.as_deref()
    }
    pub fn log_file(&self) -> Option<&str> {
        self.log_file.as_deref()
    }
    pub fn branch_name(&self) -> Option<&str> {
        self.branch_name.as_deref()
    }
    pub fn exit_code(&self) -> Option<i64> {
        self.exit_code
    }
    pub fn has_dead_process(&self) -> bool {
        self.process_alive == Some(false)
    }

    /// The watchdog's view of this Worker's current Run.
    pub fn run_progress(&self) -> RunProgress {
        RunProgress {
            status: self.status,
            started_at: self
                .started_at
                .as_deref()
                .and_then(|value| value.parse().ok()),
            log_activity_at: self.log_activity_at,
            process_alive: !self.has_dead_process(),
        }
    }

    /// The watchdog's verdict on this Worker's current Run.
    ///
    /// `now` and `window` are the caller's, not the engine's: the clock and the
    /// configured window are frontend facts, exactly as the occupied set is.
    pub fn health(&self, now: Timestamp, window: StuckWindow) -> RunHealth {
        super::watchdog::health(self.run_progress(), now, window)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotDiagnosticKind {
    MalformedPool,
    MissingWorker,
    MalformedWorker,
    /// A Pool member whose Worker Workspace directory is absent.
    MissingWorkspace,
    ReconciliationFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotDiagnostic {
    kind: SnapshotDiagnosticKind,
    path: PathBuf,
    worker_id: Option<WorkerId>,
    message: String,
}
impl SnapshotDiagnostic {
    pub fn kind(&self) -> SnapshotDiagnosticKind {
        self.kind
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn worker_id(&self) -> Option<&WorkerId> {
        self.worker_id.as_ref()
    }
    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerPoolSnapshot {
    pool: Option<PoolSnapshot>,
    workers: Vec<WorkerSnapshot>,
    diagnostics: Vec<SnapshotDiagnostic>,
    /// Worker Workspaces jjfx currently records as open in a terminal.
    occupied: BTreeSet<WorkerId>,
    /// Tickets the Dispatch capacity governor held back rather than fired.
    waiting: Vec<String>,
    /// The Pool's recorded cost and usage, or `None` when the record store could
    /// not be read (so a frontend can say "unavailable" rather than "$0").
    cost: Option<PoolCostSnapshot>,
}
impl WorkerPoolSnapshot {
    pub fn pool(&self) -> Option<&PoolSnapshot> {
        self.pool.as_ref()
    }
    pub fn workers(&self) -> &[WorkerSnapshot] {
        &self.workers
    }
    pub fn worker(&self, worker_id: &str) -> Option<&WorkerSnapshot> {
        self.workers
            .iter()
            .find(|worker| worker.worker_id.as_str() == worker_id)
    }
    pub fn diagnostics(&self) -> &[SnapshotDiagnostic] {
        &self.diagnostics
    }
    /// Returns the Worker Pool's recorded cost and usage, or `None` when the record
    /// store could not be read.
    pub fn cost(&self) -> Option<&PoolCostSnapshot> {
        self.cost.as_ref()
    }
    /// Returns the Workers recorded as open in a terminal.
    pub fn occupied(&self) -> &BTreeSet<WorkerId> {
        &self.occupied
    }
    /// Returns the Tickets the capacity governor held back from the last capped
    /// Dispatch, in the order they were requested.
    pub fn waiting(&self) -> &[String] {
        &self.waiting
    }
    /// Reports whether `worker_id` is currently open in a terminal.
    pub fn is_occupied(&self, worker_id: &str) -> bool {
        self.occupied
            .iter()
            .any(|worker| worker.as_str() == worker_id)
    }
    /// Reports whether this Pool has no state on disk at all.
    ///
    /// That is the normal state of a lane declared but never used, which
    /// materialises on first use (ADR 0013), so absence is not a diagnostic:
    /// it is inferred from the absence of a readable pool document. A pool
    /// whose state exists but could not be read is a
    /// [`SnapshotDiagnosticKind::MalformedPool`] instead, and is not missing.
    pub fn is_missing(&self) -> bool {
        self.pool.is_none()
            && !self
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.kind == SnapshotDiagnosticKind::MalformedPool)
    }
}

impl Repository {
    /// Reads this handle's Worker Pool without changing any file.
    ///
    /// A handle from [`Repository::open`] reads `default`; one narrowed with
    /// [`Repository::with_pool`] reads the named pool. The pool's own state,
    /// waiting set and Run costs are pool-scoped; the in-use marker is
    /// repository-scoped, because occupancy is one reconcile over every tab the
    /// terminal has open (ADR 0013).
    pub fn read_worker_pool_snapshot(&self) -> WorkerPoolSnapshot {
        let state = self.state_store();
        // The in-use marker is jjfx-owned presentation state, read without
        // touching the terminal; the frontend reconciles it against open tabs.
        let occupied = state.in_use().marked().unwrap_or_default();
        // The waiting set is jjfx-owned presentation state too: what the last
        // capped Dispatch held back, so a frontend can show the backlog.
        let waiting = state.waiting().held();
        // The recorded Run costs are read once here so every Pool presentation
        // carries them; an unreadable store is `None`, not zero spend.
        let cost = self.pool_cost().ok();
        let mut diagnostics = Vec::new();
        let pool = match state.pool().load() {
            // A Pool with no state is normal - a declared lane materialises on
            // first use (ADR 0013) - so absence is not a diagnostic. Only a
            // pool document that exists but cannot be read is reported; the
            // `WORKERS` column already shows `-` for a lane that has not run,
            // and narrating absence once per lane is noise on a successful
            // command.
            Ok(Loaded::Missing) => {
                return WorkerPoolSnapshot {
                    pool: None,
                    workers: Vec::new(),
                    diagnostics,
                    occupied,
                    waiting,
                    cost,
                };
            }
            Ok(Loaded::Present(versioned)) => versioned.value,
            Err(error) => {
                // A document that exists but cannot be read is a real error, so
                // it is reported - but it is named, because with several lanes a
                // user cannot otherwise tell which one is corrupt. Absence above
                // stays silent; only this unreadable case speaks.
                diagnostics.push(diagnostic(
                    self.root(),
                    SnapshotDiagnosticKind::MalformedPool,
                    None,
                    format!("Worker Pool {pool}: {error}", pool = self.selected_pool()),
                ));
                return WorkerPoolSnapshot {
                    pool: None,
                    workers: Vec::new(),
                    diagnostics,
                    occupied,
                    waiting,
                    cost,
                };
            }
        };
        let references: Vec<_> = pool
            .workers
            .iter()
            .cloned()
            .map(|worker_id| WorkerReference {
                workspace: worker_id.as_str().to_owned(),
                worker_id,
            })
            .collect();
        let snapshot = PoolSnapshot {
            size: pool.size,
            gh_repo: pool.gh_repo.clone(),
            foreground: pool.foreground,
            profile: AgentRuntimeProfile::from_configured(
                pool.agent.as_ref(),
                pool.provider.as_deref(),
                pool.model.as_deref(),
            )
            .ok(),
            workers: references,
        };
        let mut workers = Vec::new();
        for worker_id in pool.workers {
            match state.worker(worker_id.clone()).load() {
                Ok(Loaded::Missing) => diagnostics.push(diagnostic(
                    self.root(),
                    SnapshotDiagnosticKind::MissingWorker,
                    Some(worker_id),
                    "Worker state is absent",
                )),
                Err(error) => diagnostics.push(diagnostic(
                    self.root(),
                    SnapshotDiagnosticKind::MalformedWorker,
                    Some(worker_id),
                    error,
                )),
                Ok(Loaded::Present(versioned)) => match worker_snapshot(
                    worker_id.clone(),
                    pool.names.get(&worker_id).cloned().unwrap_or_default(),
                    versioned.value,
                ) {
                    Ok(worker) => workers.push(worker),
                    Err(message) => diagnostics.push(diagnostic(
                        self.root(),
                        SnapshotDiagnosticKind::MalformedWorker,
                        Some(worker_id),
                        message,
                    )),
                },
            }
        }
        WorkerPoolSnapshot {
            pool: Some(snapshot),
            workers,
            diagnostics,
            occupied,
            waiting,
            cost,
        }
    }

    /// Removes this handle's Worker Pool: terminates its recorded Runs, removes
    /// every Worker's Workspace, and removes the Worker Pool's state directory.
    ///
    /// This is the forced path - it does not drain. `pool destroy` refuses a
    /// Pool with a busy Worker or undelivered work before it reaches here unless
    /// `--force`. A Pool that never materialised state has nothing to remove and
    /// is a no-op.
    ///
    /// Unlike [`WorkerPool::destroy`], which leaves a Pool that is recreated in
    /// place, this removes the state directory outright: the Worker Pool is gone, so
    /// leaving it (and its locks) behind would be a leak that
    /// `orphan_pool_directories` would then have to report.
    pub fn remove_pool_with_progress<F>(&self, mut on_output: F) -> Result<(), WorkerPoolError>
    where
        F: FnMut(crate::engine::workspace::WorkspaceHookStream, &str),
    {
        let state_store = self.state_store();
        if !state_store.directory_exists() {
            return Ok(());
        }
        destroy_pool_state(self, &state_store, &mut on_output)?;
        state_store
            .remove_directory()
            .map_err(WorkerPoolError::State)
    }
}

fn diagnostic(
    root: &Path,
    kind: SnapshotDiagnosticKind,
    worker_id: Option<WorkerId>,
    message: impl fmt::Display,
) -> SnapshotDiagnostic {
    SnapshotDiagnostic {
        kind,
        path: root.to_owned(),
        worker_id,
        message: message.to_string(),
    }
}

fn worker_snapshot(
    worker_id: WorkerId,
    alias: String,
    state: WorkerState,
) -> Result<WorkerSnapshot, String> {
    let status = WorkerStatus::parse(&state.status)
        .ok_or_else(|| format!("unknown Worker status {:?}", state.status.as_str()))?;
    let profile = match AgentRuntimeProfile::from_run_state(
        state.agent.as_ref(),
        state.provider.as_deref(),
        state.model.as_deref(),
    ) {
        Ok(profile) => profile,
        // A Worker recorded under a removed runtime stays observable, with no
        // usable profile, so reconciliation can finalize its Run and the Worker
        // Pool view can show why it cannot run. Any other malformed runtime is
        // still reported.
        Err(_) if is_removed_worker_runtime(&state) => None,
        Err(message) => return Err(message),
    };
    let pid = state
        .pid
        .map(u32::try_from)
        .transpose()
        .map_err(|_| "Worker PID must fit a positive u32".to_owned())?;
    if pid == Some(0) {
        return Err("Worker PID must be greater than zero".to_owned());
    }
    let log_activity_at =
        super::watchdog::log_activity_at(state.log_file.as_deref().map(Path::new));
    Ok(WorkerSnapshot {
        workspace: worker_id.as_str().to_owned(),
        worker_id,
        alias,
        status,
        ticket: state.ticket,
        profile,
        started_at: state.started_at.map(|value| value.as_str().to_owned()),
        completed_at: state.completed_at.map(|value| value.as_str().to_owned()),
        log_file: state.log_file,
        branch_name: state.branch_name,
        error: state.error,
        exit_code: state.exit_code,
        process_alive: pid.map(process_is_alive),
        pid,
        log_activity_at,
    })
}

/// Reports whether the recorded PID still leads a live process group.
///
/// This is the same group-based probe reconciliation uses for the runtimes
/// whose work the PID owns (claude, codex), so the Worker Pool view and
/// reconciliation agree for them. An opencode Run is judged from its session
/// instead, because its recorded PID is only a daemon client - for opencode
/// this probe answers about the client, not the work.
#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    crate::engine::runtime::live_process_group_leader(pid).is_some()
}
#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::process::Command;
    use std::time::Duration;

    use crate::engine::{PoolCapacity, RunSupervisor, RunUsage};

    /// A throwaway Jujutsu repository with one idle Worker, so the finalization
    /// tests own a real Reservation rather than a stub.
    fn reserved_worker() -> (tempfile::TempDir, Repository, WorkerId) {
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

    fn reserve(repository: &Repository, worker: &WorkerId, ticket: &str) -> Reservation {
        repository
            .worker_pool()
            .reserve_named(worker.clone(), ticket)
            .expect("Worker reservation")
    }

    fn finalize(reservation: &Reservation, result: &RunResult) -> RunFinalization {
        reservation
            .finalize(reservation.worker_revision.clone(), result)
            .expect("Run finalization")
    }

    fn records(repository: &Repository) -> Vec<RunRecord> {
        repository
            .state_store()
            .runs()
            .records()
            .expect("Run records")
    }

    #[test]
    fn finalizing_a_run_appends_one_record_carrying_its_result() {
        let (_temporary, repository, worker) = reserved_worker();
        let reservation = reserve(&repository, &worker, "ENG-1");
        let result = RunResult::succeeded()
            .with_usage(
                RunUsage::new(10, 4)
                    .with_cached_input_tokens(2)
                    .with_cache_write_input_tokens(1)
                    .with_reasoning_output_tokens(3),
            )
            .with_cost(RunCost::from_micro_usd(1500))
            .with_duration(Duration::from_millis(2000))
            .with_turns(3);

        assert!(matches!(
            finalize(&reservation, &result),
            RunFinalization::Applied
        ));

        let records = records(&repository);
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.worker, worker);
        assert_eq!(record.ticket.as_deref(), Some("ENG-1"));
        assert_eq!(record.runtime.as_deref(), Some("claude"));
        assert_eq!(record.branch.as_deref(), Some("eng-1"));
        assert_eq!(record.cost_micro_usd, Some(1500));
        assert_eq!(record.duration_ms, Some(2000));
        assert_eq!(record.turns, Some(3));
        assert_eq!(record.conclusion, RunRecordConclusion::Succeeded);
        assert_eq!(record.error, None);
        assert!(record.run_id.contains(worker.as_str()));
        assert!(record.started_at.is_some());
        assert!(record.completed_at.is_some());
        assert!(
            record
                .workspace
                .as_deref()
                .is_some_and(|workspace| workspace.ends_with(worker.as_str()))
        );
        let usage = record.usage.expect("recorded usage");
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.cached_input_tokens, 2);
        assert_eq!(usage.cache_write_input_tokens, 1);
        assert_eq!(usage.output_tokens, 4);
        assert_eq!(usage.reasoning_output_tokens, 3);
    }

    #[test]
    fn a_run_without_a_reported_cost_still_appends_a_record() {
        let (_temporary, repository, worker) = reserved_worker();
        let reservation = reserve(&repository, &worker, "ENG-2");

        assert!(matches!(
            finalize(&reservation, &RunResult::succeeded()),
            RunFinalization::Applied
        ));

        let records = records(&repository);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].cost_micro_usd, None);
        assert_eq!(records[0].usage, None);
        assert_eq!(records[0].duration_ms, None);
        assert_eq!(records[0].turns, None);
        assert_eq!(records[0].conclusion, RunRecordConclusion::Succeeded);
    }

    #[test]
    fn a_failed_run_appends_a_record() {
        let (_temporary, repository, worker) = reserved_worker();
        let reservation = reserve(&repository, &worker, "ENG-3");

        assert!(matches!(
            finalize(&reservation, &RunResult::failed("boom")),
            RunFinalization::Applied
        ));

        let records = records(&repository);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].conclusion, RunRecordConclusion::Failed);
        assert_eq!(records[0].error.as_deref(), Some("boom"));
        assert_eq!(records[0].cost_micro_usd, None);
    }

    #[test]
    fn two_consecutive_runs_in_one_worker_both_survive() {
        let (_temporary, repository, worker) = reserved_worker();

        let first = reserve(&repository, &worker, "ENG-4");
        assert!(matches!(
            finalize(
                &first,
                &RunResult::succeeded().with_cost(RunCost::from_micro_usd(100))
            ),
            RunFinalization::Applied
        ));
        RunSupervisor::new()
            .reset_run(&repository, &worker)
            .expect("reset the finished Worker");

        let second = reserve(&repository, &worker, "ENG-5");
        assert!(matches!(
            finalize(
                &second,
                &RunResult::succeeded().with_cost(RunCost::from_micro_usd(200))
            ),
            RunFinalization::Applied
        ));

        let records = records(&repository);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].cost_micro_usd, Some(100));
        assert_eq!(records[0].ticket.as_deref(), Some("ENG-4"));
        assert_eq!(records[1].cost_micro_usd, Some(200));
        assert_eq!(records[1].ticket.as_deref(), Some("ENG-5"));
    }

    #[test]
    fn reconciling_a_finished_run_appends_exactly_one_record() {
        let (_temporary, repository, worker) = reserved_worker();
        let reservation = reserve(&repository, &worker, "ENG-7");
        // A PID that leads no live process group, so reconciliation observes the
        // Run as ended rather than still executing.
        reservation
            .persist_pid(u32::MAX - 1)
            .expect("persist a dead Run PID");

        repository.worker_pool().reconcile_runs();

        let recorded = records(&repository);
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].worker, worker);
        assert_eq!(recorded[0].ticket.as_deref(), Some("ENG-7"));

        // A later reconciliation sees the Run already finalized and records
        // nothing more.
        repository.worker_pool().reconcile_runs();
        assert_eq!(records(&repository).len(), 1);
    }

    #[test]
    fn a_run_its_supervisor_finalized_is_not_recorded_again_by_reconciliation() {
        let (_temporary, repository, worker) = reserved_worker();
        let reservation = reserve(&repository, &worker, "ENG-8");

        assert!(matches!(
            finalize(
                &reservation,
                &RunResult::succeeded().with_cost(RunCost::from_micro_usd(500))
            ),
            RunFinalization::Applied
        ));

        repository.worker_pool().reconcile_runs();

        let records = records(&repository);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].cost_micro_usd, Some(500));
    }

    #[test]
    fn a_stale_finalization_appends_nothing() {
        let (_temporary, repository, worker) = reserved_worker();
        let reservation = reserve(&repository, &worker, "ENG-6");

        assert!(matches!(
            finalize(&reservation, &RunResult::succeeded()),
            RunFinalization::Applied
        ));
        assert!(matches!(
            finalize(&reservation, &RunResult::succeeded()),
            RunFinalization::Stale
        ));

        assert_eq!(records(&repository).len(), 1);
    }
}
