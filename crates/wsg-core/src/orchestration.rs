//! Persistent, frontend-neutral Dispatch Group orchestration.
//!
//! Frontends select foreground or detached execution and render typed events.
//! This module owns orchestration order while keeping Worker Pool, Direct
//! Dispatch, compatible persistence, and terminal formatting behind one seam.

use std::path::Path;

use crate::{AgentRuntime, DispatchGroupStatusCounts, Repository, TicketId, WorkerId};

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
