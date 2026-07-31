//! Coordinated Direct Dispatch requests and outcomes.

use std::process::Command;

use thiserror::Error;

use crate::{
    AgentRuntimeInvocation, CompletedRun, DeliveryContract, DispatchBudget, DispatchPromptBuilder,
    DispatchPromptContext, DispatchPromptError, Repository, RepositoryIdentity, Reservation,
    RunMode, Ticket, WorkerId, WorkerPoolError,
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
    model: Option<String>,
    budget: DispatchBudget,
    mode: RunMode,
    dependency_context: Option<DispatchDependencyContext>,
}

impl DirectDispatchRequest {
    /// Creates a request using provider-managed model and budget behavior.
    pub fn new(ticket: Ticket, mode: RunMode) -> Self {
        Self {
            ticket,
            target: DirectDispatchTarget::FirstIdle,
            model: None,
            budget: DispatchBudget::ProviderManaged,
            mode,
            dependency_context: None,
        }
    }

    /// Selects one exact Worker and disables first-idle fallback.
    pub fn to_worker(mut self, worker: WorkerId) -> Self {
        self.target = DirectDispatchTarget::Worker(worker);
        self
    }

    /// Supplies a caller-selected model override.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        let model = model.into();
        self.model = (!model.trim().is_empty()).then_some(model);
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

    /// Returns the Ticket selected for Dispatch.
    pub fn ticket(&self) -> &Ticket {
        &self.ticket
    }

    /// Returns first-idle or exact-Worker selection.
    pub const fn target(&self) -> &DirectDispatchTarget {
        &self.target
    }

    /// Returns the optional model override.
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
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
}

/// The deep frontend-neutral coordinator for Direct Dispatch.
#[derive(Debug, Clone)]
pub struct DirectDispatch {
    repository: Repository,
}

impl DirectDispatch {
    /// Opens Direct Dispatch for one Repository.
    pub fn new(repository: Repository) -> Self {
        Self { repository }
    }

    /// Reserves capacity for one request using its deterministic target.
    ///
    /// This is the narrow Reservation handoff used by persistent orchestration
    /// before it records the Worker assignment and launches the Run.
    pub fn reserve(
        &self,
        request: &DirectDispatchRequest,
    ) -> Result<Reservation, DirectDispatchError> {
        let pool = self.repository.worker_pool();
        match request.target() {
            DirectDispatchTarget::FirstIdle => Ok(pool.reserve(request.ticket().id().as_str())?),
            DirectDispatchTarget::Worker(worker) => {
                Ok(pool.reserve_named(worker.clone(), request.ticket().id().as_str())?)
            }
        }
    }

    /// Resolves Repository delivery identity and builds one initial invocation.
    pub fn build_invocation(
        &self,
        reservation: &Reservation,
        request: &DirectDispatchRequest,
    ) -> Result<AgentRuntimeInvocation, DirectDispatchError> {
        if reservation.ticket() != request.ticket().id().as_str() {
            return Err(DirectDispatchError::ReservationTicketMismatch {
                reserved: reservation.ticket().to_owned(),
                requested: request.ticket().id().as_str().to_owned(),
            });
        }
        let repository = self.repository_identity()?;
        let assignee = self.jj_config("user.email")?;
        let user_name = self.jj_config("user.name")?;
        let branch_prefix = user_name
            .split_whitespace()
            .next()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| DirectDispatchError::Identity("jj user.name is blank".to_owned()))?
            .to_ascii_lowercase();
        let pull_request_command = match request.dependency_context() {
            Some(context) if !context.pull_request_base().trim().is_empty() => format!(
                "gh -R {} pr create --head <branch> --base {} --title \"{}: <title from Ticket>\" --body \"<summary of changes and link to Linear Ticket>\"",
                repository.as_str(),
                context.pull_request_base(),
                request.ticket().id(),
            ),
            _ => format!(
                "gh -R {} pr create --head <branch> --title \"{}: <title from Ticket>\" --body \"<summary of changes and link to Linear Ticket>\"",
                repository.as_str(),
                request.ticket().id(),
            ),
        };
        let delivery = DeliveryContract::new(assignee, branch_prefix, pull_request_command)?;
        let mut context = DispatchPromptContext::new(
            reservation.agent_runtime(),
            repository,
            request.ticket().clone(),
            delivery,
        )
        .with_budget(request.budget());
        if let Some(model) = request.model() {
            context = context.with_model(model);
        }
        if let Some(dependency) = request.dependency_context() {
            context = context.with_dependency_context(dependency.clone());
        }
        Ok(DispatchPromptBuilder::new()
            .initial(context)?
            .with_name(format!(
                "pool:{}:{}",
                reservation.worker_id(),
                request.ticket().id()
            )))
    }

    fn repository_identity(&self) -> Result<RepositoryIdentity, DirectDispatchError> {
        let configured = self
            .repository
            .worker_pool()
            .snapshot()
            .pool()
            .map(|pool| pool.gh_repo().trim_end_matches(".git").to_owned())
            .filter(|value| !value.is_empty());
        let slug = match configured {
            Some(slug) => slug,
            None => {
                let output = Command::new("jj")
                    .args(["git", "remote", "list"])
                    .current_dir(self.repository.root())
                    .output()
                    .map_err(|error| DirectDispatchError::Identity(error.to_string()))?;
                if !output.status.success() {
                    return Err(DirectDispatchError::Identity(
                        String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                    ));
                }
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .find_map(|line| {
                        let mut fields = line.split_whitespace();
                        match (fields.next(), fields.next()) {
                            (Some("origin"), Some(remote)) => Some(remote_slug(remote)),
                            _ => None,
                        }
                    })
                    .unwrap_or_default()
            }
        };
        RepositoryIdentity::parse(slug)
            .map_err(|error| DirectDispatchError::Identity(error.to_string()))
    }

    fn jj_config(&self, key: &'static str) -> Result<String, DirectDispatchError> {
        let output = Command::new("jj")
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

fn remote_slug(remote: &str) -> String {
    let remote = remote.trim_end_matches(".git");
    if let Some((_, path)) = remote.rsplit_once(':') {
        return path.to_owned();
    }
    let mut parts = remote.rsplitn(3, '/');
    match (parts.next(), parts.next()) {
        (Some(name), Some(owner)) => format!("{owner}/{name}"),
        _ => remote.to_owned(),
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
    /// Explicit partial Dispatch had no capacity for the Ticket.
    Capacity,
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

/// Ordered result for one Ticket in a Direct Dispatch batch.
#[derive(Debug)]
pub enum DirectDispatchOutcome {
    /// The Ticket was launched on its selected Worker.
    Succeeded(DirectDispatchSuccess),
    /// The Ticket failed before a usable Run was launched.
    Failed(DirectDispatchFailure),
}

impl DirectDispatchOutcome {
    /// Returns the Ticket represented by this outcome.
    pub fn ticket(&self) -> &Ticket {
        match self {
            Self::Succeeded(outcome) => outcome.ticket(),
            Self::Failed(outcome) => outcome.ticket(),
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
}
