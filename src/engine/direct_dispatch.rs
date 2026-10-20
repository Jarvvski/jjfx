//! Coordinated Direct Dispatch requests and outcomes.

use std::thread;

use thiserror::Error;

use crate::engine::pool::current_timestamp;
use crate::engine::{
    AgentModel, AgentRuntime, AgentRuntimeInvocation, AgentRuntimeProfile, BackgroundStart,
    CompletedRun, ConcurrencyCap, DeliveryContract, DispatchBudget, DispatchExclusions,
    DispatchPromptBuilder, DispatchPromptContext, DispatchPromptError, InFlightRecord,
    QueueSettlement, QueuedStep, Repository, RepositoryIdentity, Reservation, RunMode,
    RunSupervisor, RunSupervisorError, StepKey, Ticket, TicketDetails, TicketId, TicketStatus,
    TicketTitle, WorkerId, WorkerPoolError, WorkerStatus, WorkerWorkspaceError,
};

/// Ordered dependency information for a Ticket that builds on prerequisite work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchDependencyContext {
    base_revisions: Vec<String>,
    description: String,
    pull_request_base: String,
}

impl DispatchDependencyContext {
    /// Creates dependency context while preserving caller-supplied base order.
    pub fn new(
        base_revisions: Vec<String>,
        description: impl Into<String>,
        pull_request_base: impl Into<String>,
    ) -> Self {
        Self {
            base_revisions,
            description: description.into(),
            pull_request_base: pull_request_base.into(),
        }
    }

    /// Returns the ordered revisions used to prepare the Worker Workspace.
    pub fn base_revisions(&self) -> &[String] {
        &self.base_revisions
    }

    /// Returns the human-facing prerequisite summary supplied to the Agent Runtime.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns the bookmark used as the Pull Request base.
    pub fn pull_request_base(&self) -> &str {
        &self.pull_request_base
    }
}

/// Worker selection for one Direct Dispatch request.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DirectDispatchTarget {
    /// Select the first idle Worker in Pool order.
    #[default]
    FirstIdle,
    /// Require one exact Worker without fallback.
    Worker(WorkerId),
}

/// Typed inputs for one Ticket in a Direct Dispatch batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectDispatchRequest {
    ticket: Ticket,
    target: DirectDispatchTarget,
    profile: Option<AgentRuntimeProfile>,
    model: Option<AgentModel>,
    budget: DispatchBudget,
    mode: RunMode,
    dependency_context: Option<DispatchDependencyContext>,
    ticket_details: Option<TicketDetails>,
}

impl DirectDispatchRequest {
    /// Creates a request using provider-managed model and budget behavior.
    pub fn new(ticket: Ticket, mode: RunMode) -> Self {
        Self {
            ticket,
            target: DirectDispatchTarget::FirstIdle,
            profile: None,
            model: None,
            budget: DispatchBudget::ProviderManaged,
            mode,
            dependency_context: None,
            ticket_details: None,
        }
    }

    /// Creates a request from a stable Ticket ID when only the compatibility
    /// command input is available. The provider can still supply richer Ticket
    /// metadata in the Agent Runtime prompt.
    pub fn for_ticket_id(
        id: TicketId,
        mode: RunMode,
    ) -> Result<Self, crate::engine::TicketValueError> {
        let title = TicketTitle::parse(id.as_str())?;
        let status = TicketStatus::parse("Todo")?;
        Ok(Self::new(Ticket::new(id, title, status), mode))
    }

    /// Selects one exact Worker and disables first-idle fallback.
    pub fn to_worker(mut self, worker: WorkerId) -> Self {
        self.target = DirectDispatchTarget::Worker(worker);
        self
    }

    /// Supplies the persisted runtime and model profile that owns this Run.
    pub fn with_profile(mut self, profile: AgentRuntimeProfile) -> Self {
        self.profile = Some(profile);
        self
    }

    /// Supplies a caller-selected model override.
    pub fn with_model(mut self, model: impl Into<AgentModel>) -> Self {
        let model = model.into();
        self.model = (!model.model().trim().is_empty()).then_some(model);
        self
    }

    /// Supplies a caller-selected spending override.
    pub fn with_budget(mut self, budget: DispatchBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Supplies ordered dependency bases and delivery context.
    pub fn with_dependency_context(mut self, context: DispatchDependencyContext) -> Self {
        self.dependency_context = Some(context);
        self
    }

    /// Supplies the fetched Ticket title and description to inline in the prompt.
    pub fn with_ticket_details(mut self, details: TicketDetails) -> Self {
        self.ticket_details = Some(details);
        self
    }

    /// Returns the Ticket selected for Dispatch.
    pub fn ticket(&self) -> &Ticket {
        &self.ticket
    }

    /// Returns first-idle or exact-Worker selection.
    pub const fn target(&self) -> &DirectDispatchTarget {
        &self.target
    }

    /// Returns the optional persisted runtime profile override.
    pub fn profile(&self) -> Option<&AgentRuntimeProfile> {
        self.profile.as_ref()
    }

    /// Returns the optional model override.
    pub fn model(&self) -> Option<&AgentModel> {
        self.model
            .as_ref()
            .or_else(|| self.profile.as_ref().and_then(AgentRuntimeProfile::model))
    }

    /// Returns provider-managed or caller-bounded spending behavior.
    pub const fn budget(&self) -> DispatchBudget {
        self.budget
    }

    /// Returns whether the Run is attached to the caller or starts in the background.
    pub const fn mode(&self) -> RunMode {
        self.mode
    }

    /// Returns dependency-derived Workspace and delivery context.
    pub const fn dependency_context(&self) -> Option<&DispatchDependencyContext> {
        self.dependency_context.as_ref()
    }

    /// Returns the fetched Ticket details inlined into the prompt, when present.
    pub const fn ticket_details(&self) -> Option<&TicketDetails> {
        self.ticket_details.as_ref()
    }
}

/// The deep frontend-neutral coordinator for Direct Dispatch.
#[derive(Debug, Clone)]
pub struct DirectDispatch {
    repository: Repository,
    exclusions: DispatchExclusions,
    /// The optional ceiling on concurrent Runs. `None` fires whole batches, the
    /// behaviour that predates the capacity governor.
    cap: Option<ConcurrencyCap>,
}

impl DirectDispatch {
    /// Opens Direct Dispatch for one Repository.
    pub fn new(repository: Repository) -> Self {
        Self {
            repository,
            exclusions: DispatchExclusions::default(),
            cap: None,
        }
    }

    /// Excludes the given Worker Workspaces from selection for this operation.
    ///
    /// Frontends derive the set from live terminal state (open tabs); the
    /// coordinator never inspects the terminal itself.
    pub fn with_exclusions(mut self, exclusions: DispatchExclusions) -> Self {
        self.exclusions = exclusions;
        self
    }

    /// Caps how many Runs this operation may keep in flight at once.
    ///
    /// A capped batch fires its first `limit - in_flight` Tickets and holds the
    /// rest back; `None` (the default) fires the whole batch.
    pub fn with_concurrency_cap(mut self, cap: Option<ConcurrencyCap>) -> Self {
        self.cap = cap;
        self
    }

    /// Returns every finished Worker to the pool before reserving capacity.
    ///
    /// A Worker whose Run has finished reads `done`/`failed` until its outcome
    /// is reclaimed, and a Pool that never reclaims it serves only as many
    /// Tickets as it has Workers. Reclaiming here - as part of the Dispatch,
    /// before it looks for a free slot - is what makes a finished Worker
    /// available to a later Dispatch without an operator running `jjfx pool
    /// reset` between rounds. Best effort: the Reservation below reports any
    /// capacity that is still missing.
    fn reclaim_finished(&self) {
        self.repository.worker_pool().reclaim_finished();
    }

    /// Splits a batch at the capacity governor.
    ///
    /// The held-back Tickets are the tail of the requested batch, so a caller
    /// that appends them to its outcomes keeps input order. Nothing is recorded
    /// here: a batch that fails before it is launched must not leave a waiting
    /// record behind for Tickets that were never actually held.
    fn governed<'a>(
        &self,
        requests: &'a [DirectDispatchRequest],
    ) -> (&'a [DirectDispatchRequest], Vec<DirectDispatchWaiting>) {
        let Some(cap) = self.cap else {
            return (requests, Vec::new());
        };
        let in_flight = self.in_flight();
        let (fire, held) = super::governor::pace(cap, in_flight, requests);
        // The reason names the dial the owner turns and what has to change for
        // the held Ticket to run, not just the count: a held-back Ticket is a
        // "why" a human acts on, not a number to read. The count is the one that
        // actually bound the batch - the Runs already in flight plus this
        // batch's own launches - so it agrees with what the governor reports.
        let detail = super::governor::cap_hold_detail(cap, in_flight, fire.len());
        let waiting = held
            .iter()
            .map(|request| DirectDispatchWaiting::new(request.ticket().clone(), detail.clone()))
            .collect();
        (fire, waiting)
    }

    /// Records the waiting set for this batch.
    ///
    /// The set is rewritten on every batch Dispatch - including an uncapped one,
    /// which clears it - so it always describes the last Dispatch rather than
    /// accumulating Tickets that have since been fired by hand. A waiting record
    /// that cannot be written must not fail a Dispatch that is otherwise fine:
    /// it is how a frontend shows the backlog, nothing more.
    ///
    /// The waiting set is every `Waiting` outcome in the result - the governor's
    /// held-back tail and any Ticket the Pool had no free Worker for - so the two
    /// capacity gaps are recorded the same way. The Tickets are also queued
    /// durably: they are work jjfx has accepted and not yet run, so they must
    /// survive this process. Best effort, as the waiting set is - a Run in the
    /// same batch may already be live, and a queue write that failed must not
    /// misreport the Dispatch.
    fn finish(&self, result: DirectDispatchResult) -> DirectDispatchResult {
        let waiting = result.waiting();
        let _ = self.repository.state_store().waiting().replace(
            waiting
                .iter()
                .map(|held| held.ticket().id().as_str().to_owned())
                .collect(),
        );
        if let Ok(at) = current_timestamp() {
            let steps = waiting
                .iter()
                .map(|held| {
                    QueuedStep::dispatch(
                        held.ticket().id().as_str(),
                        at.clone(),
                        Some(held.detail().to_owned()),
                    )
                })
                .collect::<Vec<_>>();
            let _ = self.repository.work_queue().enqueue(&steps);
            // A step the batch already admitted carries no reason yet - `enqueue`
            // leaves a known step alone - so the reason is recorded separately.
            for held in &waiting {
                let _ = self.repository.work_queue().note(
                    &StepKey::dispatch(held.ticket().id().as_str()),
                    held.detail(),
                );
            }
        }
        result
    }

    /// The Runs already in flight: every Worker the Worker Pool records as busy.
    fn in_flight(&self) -> usize {
        self.repository
            .worker_pool()
            .snapshot()
            .workers()
            .iter()
            .filter(|worker| worker.status() == WorkerStatus::Busy)
            .count()
    }

    /// Reserves capacity for one request using its deterministic target.
    ///
    /// This is the narrow Reservation handoff used by persistent orchestration
    /// before it records the Worker assignment and launches the Run.
    pub fn reserve(
        &self,
        request: &DirectDispatchRequest,
    ) -> Result<Reservation, DirectDispatchError> {
        let profile = self.preflight(request)?;
        let worker = match request.target() {
            DirectDispatchTarget::FirstIdle => None,
            DirectDispatchTarget::Worker(worker) => Some(worker.clone()),
        };
        let reservation = self.repository.worker_pool().reserve_profiled(
            worker,
            request.ticket().id().as_str(),
            profile,
            &self.exclusions,
        )?;
        // The claim is written before the Run starts, so a jjfx killed between
        // the claim and the launch still knows what was in flight and where
        // (ticket 07). Best effort: the Reservation is real, so a queue write
        // that failed must not release it.
        if let Ok(at) = current_timestamp() {
            let step = StepKey::dispatch(request.ticket().id().as_str());
            let _ = self.repository.work_queue().enqueue(&[QueuedStep::dispatch(
                request.ticket().id().as_str(),
                at.clone(),
                None,
            )]);
            let _ = self.repository.work_queue().claim(
                &step,
                in_flight_record(
                    &self.repository,
                    reservation.worker_id(),
                    request.ticket().id().as_str(),
                    reservation.profile(),
                ),
                at,
            );
        }
        Ok(reservation)
    }

    /// Atomically reserves the complete batch before launching ordered outcomes.
    ///
    /// With a concurrency cap configured, only the Tickets that fit under it are
    /// reserved and launched; the rest are held back as waiting outcomes.
    pub fn dispatch(
        &self,
        requests: &[DirectDispatchRequest],
    ) -> Result<DirectDispatchResult, DirectDispatchError> {
        if requests.is_empty() {
            return Ok(DirectDispatchResult::default());
        }
        self.reclaim_finished();
        let (fire, waiting) = self.governed(requests);
        if fire.is_empty() {
            return Ok(self.finish(DirectDispatchResult::default().with_waiting(waiting)));
        }
        let profiles = self.preflight_all(fire)?;
        let targets = fire
            .iter()
            .zip(profiles)
            .map(|(request, profile)| {
                let worker = match request.target() {
                    DirectDispatchTarget::FirstIdle => None,
                    DirectDispatchTarget::Worker(worker) => Some(worker.clone()),
                };
                (
                    request.ticket().id().as_str().to_owned(),
                    worker,
                    Some(profile),
                )
            })
            .collect::<Vec<_>>();
        let reservations = self
            .repository
            .worker_pool()
            .reserve_targeted(&targets, &self.exclusions)?;
        let result =
            self.dispatch_claimed(fire, reservations.into_iter().enumerate().collect(), false);
        Ok(self.finish(result.with_waiting(waiting)))
    }

    /// Provisions only caller-approved missing capacity, then reserves atomically.
    pub fn dispatch_with_approved_growth(
        &self,
        requests: &[DirectDispatchRequest],
        approved_additional: usize,
    ) -> Result<DirectDispatchResult, DirectDispatchError> {
        if requests.is_empty() {
            return Ok(DirectDispatchResult::default());
        }
        self.reclaim_finished();
        let (fire, waiting) = self.governed(requests);
        if fire.is_empty() {
            return Ok(self.finish(DirectDispatchResult::default().with_waiting(waiting)));
        }
        let profiles = self.preflight_all(fire)?;
        let targets = fire
            .iter()
            .zip(profiles)
            .map(|(request, profile)| {
                let worker = match request.target() {
                    DirectDispatchTarget::FirstIdle => None,
                    DirectDispatchTarget::Worker(worker) => Some(worker.clone()),
                };
                (
                    request.ticket().id().as_str().to_owned(),
                    worker,
                    Some(profile),
                )
            })
            .collect::<Vec<_>>();
        let reservations = self.repository.worker_pool().grow_and_reserve_targeted(
            &targets,
            approved_additional,
            &self.exclusions,
        )?;
        let result =
            self.dispatch_claimed(fire, reservations.into_iter().enumerate().collect(), false);
        Ok(self.finish(result.with_waiting(waiting)))
    }

    /// Explicitly dispatches only the currently available subset of a batch.
    pub fn dispatch_use_available(
        &self,
        requests: &[DirectDispatchRequest],
    ) -> Result<DirectDispatchResult, DirectDispatchError> {
        if requests.is_empty() {
            return Ok(DirectDispatchResult::default());
        }
        self.reclaim_finished();
        let (fire, waiting) = self.governed(requests);
        if fire.is_empty() {
            return Ok(self.finish(DirectDispatchResult::default().with_waiting(waiting)));
        }
        let profiles = self.preflight_all(fire)?;
        let targets = fire
            .iter()
            .zip(profiles)
            .map(|(request, profile)| {
                let worker = match request.target() {
                    DirectDispatchTarget::FirstIdle => None,
                    DirectDispatchTarget::Worker(worker) => Some(worker.clone()),
                };
                (
                    request.ticket().id().as_str().to_owned(),
                    worker,
                    Some(profile),
                )
            })
            .collect::<Vec<_>>();
        let available = self
            .repository
            .worker_pool()
            .reserve_available_targeted(&targets, &self.exclusions)?;
        let partial = !available.unavailable.is_empty();
        let result = self.dispatch_claimed(fire, available.claimed, partial);
        Ok(self.finish(result.with_waiting(waiting)))
    }

    /// Records a whole batch as held back without firing any of it.
    ///
    /// The spend gate's block mode holds every Ticket in a discovered batch
    /// rather than firing the subset that fits: nothing is attempted and nothing
    /// is reserved, and each Ticket is written to the waiting set with the
    /// binding limit as its reason. That is the same set the capacity governor
    /// writes, so a frontend reports a spend hold exactly as it reports a
    /// capacity hold - a cap that silently dropped work would be the failure the
    /// dispatch-ergonomics batch was filed to remove.
    pub fn hold(&self, requests: &[DirectDispatchRequest], detail: &str) -> DirectDispatchResult {
        let waiting = requests
            .iter()
            .map(|request| DirectDispatchWaiting::new(request.ticket().clone(), detail))
            .collect();
        self.finish(DirectDispatchResult::default().with_waiting(waiting))
    }

    /// Launches a previously persisted Reservation and transfers its ownership.
    pub fn dispatch_reserved(
        &self,
        reservation: Reservation,
        request: &DirectDispatchRequest,
    ) -> Result<DirectDispatchSuccess, DirectDispatchError> {
        if reservation.ticket() != request.ticket().id().as_str() {
            let mismatch = DirectDispatchError::ReservationTicketMismatch {
                reserved: reservation.ticket().to_owned(),
                requested: request.ticket().id().as_str().to_owned(),
            };
            return Err(release_before_launch(&reservation, mismatch));
        }
        if let Err(error) = self.preflight_input(reservation.profile(), request) {
            return Err(release_before_launch(&reservation, error));
        }
        self.dispatch_preflighted(reservation, request)
    }

    fn dispatch_preflighted(
        &self,
        reservation: Reservation,
        request: &DirectDispatchRequest,
    ) -> Result<DirectDispatchSuccess, DirectDispatchError> {
        let worker = reservation.worker_id().clone();
        // The claim and the in-flight record are written before the Workspace is
        // prepared, so a jjfx killed between the claim and the launch still knows
        // what was in flight and where (ticket 07). Best effort: the Run may
        // follow, and a queue write that failed must not release a live Worker.
        if let Ok(at) = current_timestamp() {
            let step = StepKey::dispatch(request.ticket().id().as_str());
            let _ = self.repository.work_queue().claim(
                &step,
                in_flight_record(
                    &self.repository,
                    &worker,
                    request.ticket().id().as_str(),
                    reservation.profile(),
                ),
                at.clone(),
            );
            // Marked in flight before the launch, not after: a Foreground Run
            // finalizes inside the supervisor below, so a `begin` after it would
            // find the step already settled and never record that it started.
            let _ = self.repository.work_queue().begin(&step, None, None, at);
        }
        let bases = request
            .dependency_context()
            .map(DispatchDependencyContext::base_revisions)
            .unwrap_or_default();
        let _prepared = match self
            .repository
            .prepare_worker_workspace_for_dispatch(&worker, bases)
        {
            Ok(prepared) => prepared,
            Err(source) => {
                return Err(release_before_launch(
                    &reservation,
                    DirectDispatchError::Workspace(source),
                ));
            }
        };
        let invocation = match self.build_invocation(&reservation, request) {
            Ok(invocation) => invocation,
            Err(error) => return Err(release_before_launch(&reservation, error)),
        };
        let execution = match request.mode() {
            RunMode::Foreground => DirectDispatchExecution::Foreground(
                RunSupervisor::new().run_reserved_foreground_with_handoff(
                    reservation,
                    invocation,
                    move || drop(_prepared),
                )?,
            ),
            RunMode::Background => {
                let started =
                    RunSupervisor::new().run_reserved_background(reservation, invocation)?;
                drop(_prepared);
                match started {
                    BackgroundStart::Process(background) => {
                        let pid = background.pid();
                        thread::spawn(move || {
                            let _ = background.wait();
                        });
                        DirectDispatchExecution::Background { pid }
                    }
                    BackgroundStart::Session(session) => DirectDispatchExecution::Session {
                        session_id: session.session_id().to_owned(),
                    },
                }
            }
        };
        // The step is in flight, with the session or process only the launch
        // knows. Best effort: the Run is live, so a queue write that failed must
        // not misreport the Dispatch or release the Worker.
        // Complete the in-flight record with the session or process only the
        // launch knows. Best effort: the Run is live, so a queue write that
        // failed must not misreport the Dispatch or release the Worker.
        if let Ok(at) = current_timestamp() {
            let (session, pid) = match &execution {
                DirectDispatchExecution::Foreground(_) => (None, None),
                DirectDispatchExecution::Background { pid } => (None, Some(i64::from(*pid))),
                DirectDispatchExecution::Session { session_id } => (Some(session_id.clone()), None),
            };
            if session.is_some() || pid.is_some() {
                let _ = self.repository.work_queue().begin(
                    &StepKey::dispatch(request.ticket().id().as_str()),
                    session,
                    pid,
                    at,
                );
            }
        }
        Ok(DirectDispatchSuccess::new(
            request.ticket().clone(),
            worker,
            execution,
        ))
    }
    fn preflight_all(
        &self,
        requests: &[DirectDispatchRequest],
    ) -> Result<Vec<AgentRuntimeProfile>, DirectDispatchError> {
        let configured = self.configured_profile();
        let mut profiles = Vec::with_capacity(requests.len());
        for request in requests {
            let profile = request
                .profile()
                .cloned()
                .unwrap_or_else(|| configured.clone())
                .with_model_override(request.model());
            self.preflight_input(&profile, request)?;
            profiles.push(profile);
        }
        Ok(profiles)
    }

    fn preflight(
        &self,
        request: &DirectDispatchRequest,
    ) -> Result<AgentRuntimeProfile, DirectDispatchError> {
        let profile = request
            .profile()
            .cloned()
            .unwrap_or_else(|| self.configured_profile())
            .with_model_override(request.model());
        self.preflight_input(&profile, request)?;
        Ok(profile)
    }

    fn configured_profile(&self) -> AgentRuntimeProfile {
        self.repository
            .worker_pool()
            .snapshot()
            .pool()
            .and_then(|pool| pool.profile().cloned())
            .unwrap_or_else(|| AgentRuntimeProfile::new(AgentRuntime::Claude))
    }

    fn preflight_input(
        &self,
        profile: &AgentRuntimeProfile,
        request: &DirectDispatchRequest,
    ) -> Result<(), DirectDispatchError> {
        if matches!(request.budget(), DispatchBudget::MaximumUsd(_))
            && profile.runtime() != AgentRuntime::Claude
        {
            return Err(DispatchPromptError::UnsupportedBudget {
                runtime: profile.runtime(),
            }
            .into());
        }
        Ok(())
    }

    fn dispatch_claimed(
        &self,
        requests: &[DirectDispatchRequest],
        claimed: Vec<(usize, Reservation)>,
        partial: bool,
    ) -> DirectDispatchResult {
        self.queue_batch(requests);
        // Every Ticket starts as waiting for capacity: a Reservation replaces it
        // with a launched Run, and one the Pool has no free Worker for stays
        // waiting - the work is outstanding, not failed. A batch only reaches
        // here with a Ticket left over when it is an explicit use-available
        // Dispatch, so the whole batch fires when the Pool has room.
        let mut outcomes = requests
            .iter()
            .map(|request| {
                DirectDispatchOutcome::Waiting(DirectDispatchWaiting::new(
                    request.ticket().clone(),
                    super::governor::POOL_CAPACITY_GAP_DETAIL,
                ))
            })
            .collect::<Vec<_>>();
        for (index, reservation) in claimed {
            let worker = reservation.worker_id().clone();
            outcomes[index] = match self.dispatch_preflighted(reservation, &requests[index]) {
                Ok(success) => DirectDispatchOutcome::Succeeded(success),
                Err(error) => DirectDispatchOutcome::Failed(DirectDispatchFailure::new(
                    requests[index].ticket().clone(),
                    Some(worker),
                    failure_phase(&error),
                    error.to_string(),
                )),
            };
        }
        self.record_outcomes(&outcomes);
        DirectDispatchResult::new(outcomes, partial)
    }

    /// Records every requested step in the durable queue before anything is
    /// claimed.
    ///
    /// A step must be queued before a claim or a launch can move it, and the
    /// whole batch is admitted at once so the queue order is the caller's input
    /// order rather than the order launches happened to finish in.
    ///
    /// Best effort: a Run in this batch may already be live, so a queue write
    /// that failed must not misreport the Dispatch.
    fn queue_batch(&self, requests: &[DirectDispatchRequest]) {
        let Ok(at) = current_timestamp() else {
            return;
        };
        let steps = requests
            .iter()
            .map(|request| QueuedStep::dispatch(request.ticket().id().as_str(), at.clone(), None))
            .collect::<Vec<_>>();
        let _ = self.repository.work_queue().enqueue(&steps);
    }

    /// Records the durable queue outcome of every Ticket in one batch.
    ///
    /// A Ticket that never started - the governor held it, or the Pool had no
    /// free Worker - is queued, not failed: the work is outstanding, and
    /// [`Self::finish`] records it. One that was claimed and failed before its
    /// Run started is settled as failed, so "what happened to this?" has an
    /// answer.
    ///
    /// Best effort: a Run in this batch may already be live, so a queue write
    /// that failed must not misreport the Dispatch.
    fn record_outcomes(&self, outcomes: &[DirectDispatchOutcome]) {
        for outcome in outcomes {
            let DirectDispatchOutcome::Failed(failure) = outcome else {
                continue;
            };
            let Ok(at) = current_timestamp() else {
                continue;
            };
            let _ = self.repository.work_queue().settle(
                &StepKey::dispatch(failure.ticket().id().as_str()),
                QueueSettlement::Failed {
                    detail: failure.detail().to_owned(),
                },
                at,
            );
        }
    }

    fn build_invocation(
        &self,
        reservation: &Reservation,
        request: &DirectDispatchRequest,
    ) -> Result<AgentRuntimeInvocation, DirectDispatchError> {
        let repository = self.repository_identity()?;
        let assignee = self.jj_config("user.email")?;
        let user_name = self.jj_config("user.name")?;
        let branch_prefix = user_name
            .split_whitespace()
            .next()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| DirectDispatchError::Identity("jj user.name is blank".to_owned()))?
            .to_ascii_lowercase();
        let delivery = DeliveryContract::new(assignee, branch_prefix)?;
        let mut context = DispatchPromptContext::new(
            reservation.agent_runtime(),
            repository,
            request.ticket().clone(),
            delivery,
        )
        .with_budget(request.budget());
        if let Some(model) = reservation.profile().model() {
            context = context.with_model(model.clone());
        }
        if let Some(dependency) = request.dependency_context() {
            context = context.with_dependency_context(dependency.clone());
        }
        if let Some(details) = request.ticket_details() {
            context = context.with_ticket_details(details.clone());
        }
        Ok(DispatchPromptBuilder::new().initial(context)?.with_name(
            crate::engine::opencode_api::run_session_title(
                reservation.worker_id(),
                request.ticket().id().as_str(),
            ),
        ))
    }

    fn repository_identity(&self) -> Result<RepositoryIdentity, DirectDispatchError> {
        let snapshot = self.repository.worker_pool().snapshot();
        let configured = snapshot.pool().map_or("", |pool| pool.gh_repo());
        crate::engine::workspace::repository_identity(self.repository.root(), configured)
            .map_err(|error| DirectDispatchError::Identity(error.to_string()))
    }

    fn jj_config(&self, key: &'static str) -> Result<String, DirectDispatchError> {
        let output = crate::jj::command()
            .args(["config", "get", key])
            .current_dir(self.repository.root())
            .output()
            .map_err(|error| DirectDispatchError::Identity(format!("read jj {key}: {error}")))?;
        if !output.status.success() {
            return Err(DirectDispatchError::Identity(format!(
                "read jj {key}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if value.is_empty() {
            return Err(DirectDispatchError::Identity(format!("jj {key} is blank")));
        }
        Ok(value)
    }
}

/// The in-flight record a claim leaves behind, for a resumption (ticket 07).
///
/// Everything here is known once the Reservation exists - the Worker, its
/// Workspace, the Ticket's bookmark and the selected profile - so a jjfx that
/// dies before the Run reports back can still say what was in flight. Only the
/// session or process is left for the launch to add.
fn in_flight_record(
    repository: &Repository,
    worker: &WorkerId,
    ticket: &str,
    profile: &AgentRuntimeProfile,
) -> InFlightRecord {
    InFlightRecord::for_run(
        worker.clone(),
        repository
            .worker_workspace_path(worker)
            .to_string_lossy()
            .into_owned(),
        ticket.to_lowercase(),
        profile,
    )
}

fn failure_phase(error: &DirectDispatchError) -> DirectDispatchFailurePhase {
    match error {
        DirectDispatchError::Workspace(_) => DirectDispatchFailurePhase::Workspace,
        DirectDispatchError::Identity(_) => DirectDispatchFailurePhase::Identity,
        DirectDispatchError::Prompt(_) => DirectDispatchFailurePhase::Prompt,
        DirectDispatchError::ReservationRelease { primary, .. } => failure_phase(primary),
        DirectDispatchError::WorkerPool(_)
        | DirectDispatchError::ReservationTicketMismatch { .. }
        | DirectDispatchError::Runtime(_) => DirectDispatchFailurePhase::Launch,
    }
}

fn release_before_launch(
    reservation: &Reservation,
    primary: DirectDispatchError,
) -> DirectDispatchError {
    match reservation.release() {
        Ok(()) => primary,
        Err(release) => DirectDispatchError::ReservationRelease {
            primary: Box::new(primary),
            release,
        },
    }
}

/// Failures that prevent Direct Dispatch coordination from starting or completing.
#[derive(Debug, Error)]
pub enum DirectDispatchError {
    /// Worker Pool Reservation or lifecycle mutation failed.
    #[error(transparent)]
    WorkerPool(#[from] WorkerPoolError),
    /// Repository or jj delivery identity could not be resolved.
    #[error("cannot resolve Direct Dispatch identity: {0}")]
    Identity(String),
    /// The Reservation belongs to another Ticket.
    #[error("Reservation belongs to Ticket {reserved}, not requested Ticket {requested}")]
    ReservationTicketMismatch { reserved: String, requested: String },
    /// Typed initial prompt construction failed.
    #[error(transparent)]
    Prompt(#[from] DispatchPromptError),
    /// Worker Workspace preparation failed.
    #[error(transparent)]
    Workspace(#[from] WorkerWorkspaceError),
    /// Agent Runtime launch or supervision failed.
    #[error(transparent)]
    Runtime(#[from] RunSupervisorError),
    /// Pre-launch compensation conflicted while preserving the primary failure.
    #[error("{primary}; additionally failed to release Reservation: {release}")]
    ReservationRelease {
        primary: Box<DirectDispatchError>,
        release: WorkerPoolError,
    },
}

/// The execution side of one successfully launched Direct Dispatch.
#[derive(Debug)]
pub enum DirectDispatchExecution {
    /// The foreground Run completed and finalized before Dispatch returned.
    Foreground(CompletedRun),
    /// The background process identifier was persisted before Dispatch returned.
    Background {
        /// Process and process-group identifier for the Agent Runtime.
        pid: u32,
    },
    /// The prompt reached the Run's opencode session through the daemon API, so
    /// no client process was spawned. Reconciliation finishes the Run on the
    /// session's outcome.
    Session {
        /// The session the prompt was delivered to.
        session_id: String,
    },
}

/// One successful per-Ticket Direct Dispatch outcome.
#[derive(Debug)]
pub struct DirectDispatchSuccess {
    ticket: Ticket,
    worker: WorkerId,
    execution: DirectDispatchExecution,
}

impl DirectDispatchSuccess {
    /// Creates a successful outcome for an already selected Worker.
    pub fn new(ticket: Ticket, worker: WorkerId, execution: DirectDispatchExecution) -> Self {
        Self {
            ticket,
            worker,
            execution,
        }
    }

    /// Returns the Ticket routed by this Dispatch.
    pub fn ticket(&self) -> &Ticket {
        &self.ticket
    }

    /// Returns the Worker selected for the Ticket.
    pub fn worker(&self) -> &WorkerId {
        &self.worker
    }

    /// Returns foreground completion or the persisted background PID.
    pub const fn execution(&self) -> &DirectDispatchExecution {
        &self.execution
    }

    /// Consumes the outcome and returns its execution value.
    pub fn into_execution(self) -> DirectDispatchExecution {
        self.execution
    }
}

/// The phase at which one Ticket failed to Dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectDispatchFailurePhase {
    /// Worker Workspace preparation failed.
    Workspace,
    /// Repository or delivery identity resolution failed.
    Identity,
    /// Initial prompt construction failed.
    Prompt,
    /// Agent Runtime launch failed.
    Launch,
}

impl DirectDispatchFailurePhase {
    /// Returns the stable lowercase name used in machine-readable output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Identity => "identity",
            Self::Prompt => "prompt",
            Self::Launch => "launch",
        }
    }
}

/// One failed per-Ticket Direct Dispatch outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectDispatchFailure {
    ticket: Ticket,
    worker: Option<WorkerId>,
    phase: DirectDispatchFailurePhase,
    detail: String,
}

impl DirectDispatchFailure {
    /// Creates a failed outcome with optional selected-Worker context.
    pub fn new(
        ticket: Ticket,
        worker: Option<WorkerId>,
        phase: DirectDispatchFailurePhase,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            ticket,
            worker,
            phase,
            detail: detail.into(),
        }
    }

    /// Returns the Ticket that could not be dispatched.
    pub fn ticket(&self) -> &Ticket {
        &self.ticket
    }

    /// Returns the selected Worker when capacity had already been reserved.
    pub const fn worker(&self) -> Option<&WorkerId> {
        self.worker.as_ref()
    }

    /// Returns the failed coordination phase.
    pub const fn phase(&self) -> DirectDispatchFailurePhase {
        self.phase
    }

    /// Returns contextual failure detail.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

/// One Ticket the capacity governor held back rather than fired.
///
/// Not a failure: nothing was attempted and nothing was reserved. The Ticket is
/// waiting for a Run to finish and free a slot under the configured cap, so the
/// owner can dispatch it again (or let the next Dispatch pick it up).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectDispatchWaiting {
    ticket: Ticket,
    detail: String,
}

impl DirectDispatchWaiting {
    /// Creates a waiting outcome with the reason it was held back.
    pub fn new(ticket: Ticket, detail: impl Into<String>) -> Self {
        Self {
            ticket,
            detail: detail.into(),
        }
    }

    /// Returns the Ticket that is waiting.
    pub fn ticket(&self) -> &Ticket {
        &self.ticket
    }

    /// Returns why the Ticket was held back.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

/// Ordered result for one Ticket in a Direct Dispatch batch.
#[derive(Debug)]
pub enum DirectDispatchOutcome {
    /// The Ticket was launched on its selected Worker.
    Succeeded(DirectDispatchSuccess),
    /// The Ticket failed before a usable Run was launched.
    Failed(DirectDispatchFailure),
    /// The capacity governor held the Ticket back; nothing was attempted.
    Waiting(DirectDispatchWaiting),
}

impl DirectDispatchOutcome {
    /// Returns the Ticket represented by this outcome.
    pub fn ticket(&self) -> &Ticket {
        match self {
            Self::Succeeded(outcome) => outcome.ticket(),
            Self::Failed(outcome) => outcome.ticket(),
            Self::Waiting(outcome) => outcome.ticket(),
        }
    }
}

/// Ordered outcomes for one Direct Dispatch request batch.
#[derive(Debug, Default)]
pub struct DirectDispatchResult {
    outcomes: Vec<DirectDispatchOutcome>,
    partial: bool,
}

impl DirectDispatchResult {
    /// Creates an ordered batch result.
    pub fn new(outcomes: Vec<DirectDispatchOutcome>, partial: bool) -> Self {
        Self { outcomes, partial }
    }

    /// Returns one outcome per input Ticket in input order.
    pub fn outcomes(&self) -> &[DirectDispatchOutcome] {
        &self.outcomes
    }

    /// Reports that an explicit use-available request left Tickets undispatched.
    pub const fn is_partial(&self) -> bool {
        self.partial
    }

    /// Returns the Tickets the capacity governor held back, in input order.
    pub fn waiting(&self) -> Vec<&DirectDispatchWaiting> {
        self.outcomes
            .iter()
            .filter_map(|outcome| match outcome {
                DirectDispatchOutcome::Waiting(waiting) => Some(waiting),
                _ => None,
            })
            .collect()
    }

    /// Appends the Tickets the capacity governor held back.
    ///
    /// They are the tail of the requested batch, so appending keeps the outcomes
    /// in the order the caller asked for them. Held-back Tickets do not make the
    /// result partial: `partial` means an explicit use-available Dispatch could
    /// not find capacity, which is a different answer from "you asked for more
    /// than the cap allows at once".
    pub fn with_waiting(mut self, waiting: Vec<DirectDispatchWaiting>) -> Self {
        self.outcomes
            .extend(waiting.into_iter().map(DirectDispatchOutcome::Waiting));
        self
    }
}
