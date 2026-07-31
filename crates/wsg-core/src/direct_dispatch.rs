//! Coordinated Direct Dispatch requests and outcomes.

use crate::{CompletedRun, DispatchBudget, RunMode, Ticket, WorkerId};

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

/// Typed inputs for one Ticket in a Direct Dispatch batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectDispatchRequest {
    ticket: Ticket,
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
            model: None,
            budget: DispatchBudget::ProviderManaged,
            mode,
            dependency_context: None,
        }
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
