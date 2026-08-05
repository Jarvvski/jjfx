//! Pure Dispatch Group dependency progression and lifecycle rules.
//!
//! This module is deliberately separate from compatible state persistence. The
//! aggregate accepts and returns the wire state, but never reads files, clocks,
//! processes, Linear, or terminal output.

use std::collections::BTreeSet;
use std::fmt;

use thiserror::Error;

use crate::{
    DependencyGraph, DispatchDependencyContext, DispatchGroupOptions, DispatchGroupState,
    SubIssueState, TicketId, WireStatus, WireTimestamp, WorkerId,
};

/// The five Sub-issue statuses persisted by Go wsg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubIssueStatus {
    /// Eligible for dispatch once its blockers are satisfied.
    Pending,
    /// Assigned to a Worker with a Run in progress.
    Dispatched,
    /// Completed successfully.
    Done,
    /// Exhausted its retry policy after a failed Run.
    Failed,
    /// Excluded from execution, including already delivered or cross-repository work.
    Skipped,
}

impl SubIssueStatus {
    /// Returns the exact lowercase value used in compatible state files.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Dispatched => "dispatched",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }

    /// Returns whether the status is still active.
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Pending | Self::Dispatched)
    }

    /// Returns whether the status is terminal.
    pub const fn is_terminal(self) -> bool {
        !self.is_active()
    }

    /// Returns whether the status satisfies a downstream dependency.
    pub const fn unblocks(self) -> bool {
        matches!(self, Self::Done | Self::Skipped)
    }
}

impl TryFrom<&WireStatus> for SubIssueStatus {
    type Error = UnknownSubIssueStatus;

    fn try_from(status: &WireStatus) -> Result<Self, Self::Error> {
        match status.as_str() {
            "pending" => Ok(Self::Pending),
            "dispatched" => Ok(Self::Dispatched),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            "skipped" => Ok(Self::Skipped),
            _ => Err(UnknownSubIssueStatus(status.as_str().to_owned())),
        }
    }
}

impl From<SubIssueStatus> for WireStatus {
    fn from(status: SubIssueStatus) -> Self {
        Self::new(status.as_str())
    }
}

/// A persisted Sub-issue status outside the compatibility vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown Dispatch Group Sub-issue status {0:?}")]
pub struct UnknownSubIssueStatus(String);

impl UnknownSubIssueStatus {
    /// Returns the unrecognized wire spelling.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Inputs required to construct a Dispatch Group without performing I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchGroupBuildOptions {
    created_at: WireTimestamp,
    gh_repo: String,
    opts: DispatchGroupOptions,
}

impl DispatchGroupBuildOptions {
    /// Creates construction metadata from caller-owned repository and provider values.
    pub fn new(
        created_at: WireTimestamp,
        gh_repo: impl Into<String>,
        opts: DispatchGroupOptions,
    ) -> Self {
        Self {
            created_at,
            gh_repo: gh_repo.into(),
            opts,
        }
    }
}

/// Counts of terminal Sub-issue outcomes in a Dispatch Group.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DispatchGroupStatusCounts {
    done: usize,
    failed: usize,
    skipped: usize,
}

impl DispatchGroupStatusCounts {
    /// Returns the number of successfully completed Sub-issues.
    pub const fn done(self) -> usize {
        self.done
    }

    /// Returns the number of exhausted failed Sub-issues.
    pub const fn failed(self) -> usize {
        self.failed
    }

    /// Returns the number of skipped Sub-issues.
    pub const fn skipped(self) -> usize {
        self.skipped
    }
}

/// A pure lifecycle event applied to one Sub-issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchGroupEvent {
    /// Assigns a pending Ticket to a Worker before launch.
    Dispatched {
        /// Ticket being launched.
        ticket: TicketId,
        /// Worker reserved for the launch.
        worker: WorkerId,
        /// Caller-provided dispatch timestamp.
        at: WireTimestamp,
    },
    /// Returns a persisted assignment to pending after launch compensation.
    DispatchAborted {
        /// Ticket whose Run never started.
        ticket: TicketId,
        /// Worker whose Reservation was released.
        worker: WorkerId,
    },
    /// Records a successful Run outcome.
    Completed {
        /// Ticket whose Run completed.
        ticket: TicketId,
        /// Worker that ran the Ticket.
        worker: WorkerId,
        /// Resulting bookmark, when one was created.
        branch: Option<String>,
        /// Caller-provided completion timestamp.
        at: WireTimestamp,
    },
    /// Records a failed Run observation.
    Failed {
        /// Ticket whose Run failed.
        ticket: TicketId,
        /// Worker that ran the Ticket.
        worker: WorkerId,
        /// Caller-provided failure timestamp.
        at: WireTimestamp,
    },
    /// Makes a first failed Run pending again after Worker Reset succeeds.
    Retried {
        /// Ticket becoming dispatchable again.
        ticket: TicketId,
        /// Worker that was reset.
        worker: WorkerId,
    },
    /// Records work delivered outside a Worker Run.
    Merged {
        /// Ticket now present on the main branch.
        ticket: TicketId,
        /// Caller-provided delivery timestamp.
        at: WireTimestamp,
    },
}

/// The observable result of a successful aggregate transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchGroupTransition {
    /// A Ticket was recorded as dispatched.
    Dispatched,
    /// A pre-launch assignment was returned to pending.
    DispatchAborted,
    /// A Ticket was recorded as completed.
    Completed,
    /// A first failure needs a successful Worker Reset before retry.
    RetryRequired,
    /// A failed Ticket was returned to pending after Reset.
    Retried,
    /// A Ticket reached terminal failure.
    Failed,
    /// A Ticket was recorded as merged on main.
    Merged,
}

/// The pure Dispatch Group aggregate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchGroup {
    state: DispatchGroupState,
}

impl DispatchGroup {
    /// Builds a group from a validated Ticket dependency graph.
    pub fn from_dependency_graph(
        graph: &DependencyGraph,
        options: DispatchGroupBuildOptions,
    ) -> Result<Self, DispatchGroupError> {
        let mut state = DispatchGroupState::new(
            graph.parent().id().clone(),
            options.created_at,
            options.gh_repo,
            options.opts,
        );
        for (id, discovered) in graph.sub_issues() {
            let blocked_by = discovered
                .blockers()
                .iter()
                .map(|blocker| blocker.id().clone())
                .collect();
            let mut issue = SubIssueState::new(
                discovered.ticket().title().as_str(),
                WireStatus::new("pending"),
                blocked_by,
            );
            if discovered.is_cross_repository() {
                issue.status = WireStatus::new("skipped");
                issue.skip_reason = Some("cross-repo".to_owned());
            } else if !is_dispatchable_status(discovered.ticket().status().as_str()) {
                issue.status = WireStatus::new("skipped");
                issue.skip_reason = Some(discovered.ticket().status().as_str().to_owned());
                if is_merged_status(discovered.ticket().status().as_str()) {
                    issue.branch = Some("main".to_owned());
                }
            }
            state.sub_issues.insert(id.clone(), issue);
        }
        Self::from_state(state)
    }

    /// Wraps a compatible state after validating its domain status vocabulary.
    pub fn from_state(state: DispatchGroupState) -> Result<Self, DispatchGroupError> {
        validate_state(&state)?;
        Ok(Self { state })
    }

    /// Applies one pure lifecycle event and returns the resulting transition.
    pub fn apply(
        &mut self,
        event: DispatchGroupEvent,
    ) -> Result<DispatchGroupTransition, DispatchGroupError> {
        match event {
            DispatchGroupEvent::Dispatched { ticket, worker, at } => {
                if self.state.sub_issues.iter().any(|(other, other_issue)| {
                    other != &ticket
                        && status_of(other_issue) == SubIssueStatus::Dispatched
                        && other_issue.worker.as_ref() == Some(&worker)
                }) {
                    return Err(DispatchGroupError::Invalid(format!(
                        "Worker {worker} is already assigned to an active Ticket"
                    )));
                }
                let ready = self.ready();
                let issue = self.issue_mut(&ticket)?;
                if status_of(issue) != SubIssueStatus::Pending
                    || issue.worker.is_some()
                    || !ready.contains(&ticket)
                {
                    return Err(DispatchGroupError::Invalid(format!(
                        "Ticket {ticket} is not dispatchable"
                    )));
                }
                issue.status = WireStatus::new("dispatched");
                issue.worker = Some(worker);
                issue.dispatched_at = Some(at);
                issue.completed_at = None;
                Ok(DispatchGroupTransition::Dispatched)
            }
            DispatchGroupEvent::DispatchAborted { ticket, worker } => {
                let issue = self.issue_mut(&ticket)?;
                ensure_assigned(issue, &ticket, &worker)?;
                issue.status = WireStatus::new("pending");
                issue.worker = None;
                issue.dispatched_at = None;
                issue.completed_at = None;
                Ok(DispatchGroupTransition::DispatchAborted)
            }
            DispatchGroupEvent::Completed {
                ticket,
                worker,
                branch,
                at,
            } => {
                let issue = self.issue_mut(&ticket)?;
                ensure_assigned(issue, &ticket, &worker)?;
                issue.status = WireStatus::new("done");
                issue.completed_at = Some(at);
                issue.branch = branch.filter(|branch| !branch.is_empty());
                Ok(DispatchGroupTransition::Completed)
            }
            DispatchGroupEvent::Failed { ticket, worker, at } => {
                let issue = self.issue_mut(&ticket)?;
                ensure_assigned(issue, &ticket, &worker)?;
                if issue.retries < 1 {
                    return Ok(DispatchGroupTransition::RetryRequired);
                }
                issue.status = WireStatus::new("failed");
                issue.completed_at = Some(at);
                Ok(DispatchGroupTransition::Failed)
            }
            DispatchGroupEvent::Retried { ticket, worker } => {
                let issue = self.issue_mut(&ticket)?;
                ensure_assigned(issue, &ticket, &worker)?;
                if issue.retries >= 1 {
                    return Err(DispatchGroupError::Invalid(format!(
                        "Ticket {ticket} has exhausted its retry allowance"
                    )));
                }
                issue.status = WireStatus::new("pending");
                issue.worker = None;
                issue.dispatched_at = None;
                issue.completed_at = None;
                issue.retries += 1;
                Ok(DispatchGroupTransition::Retried)
            }
            DispatchGroupEvent::Merged { ticket, at } => {
                let issue = self.issue_mut(&ticket)?;
                if status_of(issue) == SubIssueStatus::Dispatched {
                    return Err(DispatchGroupError::Invalid(format!(
                        "dispatched Ticket {ticket} cannot be marked merged"
                    )));
                }
                issue.status = WireStatus::new("skipped");
                issue.skip_reason = Some("merged".to_owned());
                issue.branch = Some("main".to_owned());
                issue.completed_at = Some(at);
                Ok(DispatchGroupTransition::Merged)
            }
        }
    }

    /// Returns pending Tickets whose direct blockers all satisfy their dependencies.
    pub fn ready(&self) -> Vec<crate::TicketId> {
        self.state
            .sub_issues
            .iter()
            .filter_map(|(id, issue)| {
                (status_of(issue) == SubIssueStatus::Pending
                    && issue.blocked_by.iter().all(|blocker| {
                        self.state
                            .sub_issues
                            .get(blocker)
                            .is_none_or(|dependency| status_of(dependency).unblocks())
                    }))
                .then_some(id.clone())
            })
            .collect()
    }

    /// Builds stacked-branch context for a Ticket from its direct blockers.
    pub fn dependency_context(
        &self,
        ticket: &crate::TicketId,
    ) -> Result<Option<DispatchDependencyContext>, DispatchGroupError> {
        let issue = self
            .state
            .sub_issues
            .get(ticket)
            .ok_or_else(|| DispatchGroupError::UnknownTicket(ticket.to_string()))?;
        let branches = issue
            .blocked_by
            .iter()
            .filter_map(|blocker| {
                self.state
                    .sub_issues
                    .get(blocker)
                    .and_then(|dependency| dependency.branch.clone())
            })
            .collect::<Vec<_>>();
        if branches.is_empty() || branches.iter().all(|branch| branch == "main") {
            return Ok(None);
        }
        let description = issue
            .blocked_by
            .iter()
            .filter_map(|blocker| {
                let dependency = self.state.sub_issues.get(blocker)?;
                let branch = dependency.branch.as_deref()?;
                (!branch.is_empty() && branch != "main").then(|| {
                    format!(
                        "- Branch: {branch} (implements {blocker}: \"{}\")",
                        dependency.title
                    )
                })
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(Some(DispatchDependencyContext::new(
            branches.clone(),
            description,
            branches[0].clone(),
        )))
    }

    /// Returns whether every Sub-issue is terminal.
    pub fn is_terminal(&self) -> bool {
        self.state
            .sub_issues
            .values()
            .all(|issue| status_of(issue).is_terminal())
    }

    /// Counts done, failed, and skipped Sub-issues.
    pub fn status_counts(&self) -> DispatchGroupStatusCounts {
        self.state.sub_issues.values().fold(
            DispatchGroupStatusCounts::default(),
            |mut counts, issue| {
                match status_of(issue) {
                    SubIssueStatus::Done => counts.done += 1,
                    SubIssueStatus::Failed => counts.failed += 1,
                    SubIssueStatus::Skipped => counts.skipped += 1,
                    SubIssueStatus::Pending | SubIssueStatus::Dispatched => {}
                }
                counts
            },
        )
    }

    /// Returns the largest dependency wave in the group's graph.
    pub fn maximum_wave_size(&self) -> usize {
        let mut resolved = self
            .state
            .sub_issues
            .iter()
            .filter_map(|(id, issue)| {
                (status_of(issue) == SubIssueStatus::Skipped).then_some(id.clone())
            })
            .collect::<std::collections::BTreeSet<_>>();
        let mut maximum = 0;
        loop {
            let wave = self
                .state
                .sub_issues
                .iter()
                .filter_map(|(id, issue)| {
                    if resolved.contains(id) || status_of(issue) == SubIssueStatus::Skipped {
                        return None;
                    }
                    issue
                        .blocked_by
                        .iter()
                        .all(|blocker| resolved.contains(blocker))
                        .then_some(id.clone())
                })
                .collect::<Vec<_>>();
            if wave.is_empty() {
                break;
            }
            maximum = maximum.max(wave.len());
            resolved.extend(wave);
        }
        maximum
    }

    fn issue_mut(&mut self, ticket: &TicketId) -> Result<&mut SubIssueState, DispatchGroupError> {
        self.state
            .sub_issues
            .get_mut(ticket)
            .ok_or_else(|| DispatchGroupError::UnknownTicket(ticket.to_string()))
    }

    /// Returns the compatible state without performing persistence.
    pub fn into_state(self) -> DispatchGroupState {
        self.state
    }

    /// Borrows the compatible state for inspection by an adapter.
    pub fn state(&self) -> &DispatchGroupState {
        &self.state
    }
}

fn validate_state(state: &DispatchGroupState) -> Result<(), DispatchGroupError> {
    let mut active_workers = BTreeSet::new();
    for (ticket, issue) in &state.sub_issues {
        let status = SubIssueStatus::try_from(&issue.status)?;
        if issue.retries < 0 || issue.retries > 1 {
            return Err(DispatchGroupError::Invalid(format!(
                "Ticket {ticket} has invalid retry count {}",
                issue.retries
            )));
        }
        let mut blockers = BTreeSet::new();
        for blocker in &issue.blocked_by {
            if blocker == ticket {
                return Err(DispatchGroupError::Invalid(format!(
                    "Ticket {ticket} cannot block itself"
                )));
            }
            if !blockers.insert(blocker) {
                return Err(DispatchGroupError::Invalid(format!(
                    "Ticket {ticket} lists Blocker {blocker} more than once"
                )));
            }
            if !state.sub_issues.contains_key(blocker) {
                return Err(DispatchGroupError::Invalid(format!(
                    "Ticket {ticket} has unknown Blocker {blocker}"
                )));
            }
        }
        if status == SubIssueStatus::Dispatched {
            let worker = issue.worker.as_ref().ok_or_else(|| {
                DispatchGroupError::Invalid(format!("dispatched Ticket {ticket} has no Worker"))
            })?;
            if issue.dispatched_at.is_none() {
                return Err(DispatchGroupError::Invalid(format!(
                    "dispatched Ticket {ticket} has no dispatch timestamp"
                )));
            }
            if !active_workers.insert(worker.clone()) {
                return Err(DispatchGroupError::Invalid(format!(
                    "Worker {worker} is assigned to multiple active Tickets"
                )));
            }
        } else if status == SubIssueStatus::Pending && issue.worker.is_some() {
            return Err(DispatchGroupError::Invalid(format!(
                "pending Ticket {ticket} still has a Worker"
            )));
        }
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for ticket in state.sub_issues.keys() {
        validate_acyclic(ticket, state, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn validate_acyclic(
    ticket: &TicketId,
    state: &DispatchGroupState,
    visiting: &mut BTreeSet<TicketId>,
    visited: &mut BTreeSet<TicketId>,
) -> Result<(), DispatchGroupError> {
    if visited.contains(ticket) {
        return Ok(());
    }
    if !visiting.insert(ticket.clone()) {
        return Err(DispatchGroupError::Invalid(format!(
            "dependency cycle includes Ticket {ticket}"
        )));
    }
    let Some(issue) = state.sub_issues.get(ticket) else {
        return Err(DispatchGroupError::Invalid(format!(
            "dependency references unknown Ticket {ticket}"
        )));
    };
    for blocker in &issue.blocked_by {
        validate_acyclic(blocker, state, visiting, visited)?;
    }
    visiting.remove(ticket);
    visited.insert(ticket.clone());
    Ok(())
}

fn ensure_assigned(
    issue: &SubIssueState,
    ticket: &TicketId,
    worker: &WorkerId,
) -> Result<(), DispatchGroupError> {
    if status_of(issue) != SubIssueStatus::Dispatched || issue.worker.as_ref() != Some(worker) {
        return Err(DispatchGroupError::Invalid(format!(
            "Ticket {ticket} is not assigned to Worker {worker}"
        )));
    }
    Ok(())
}

fn status_of(issue: &SubIssueState) -> SubIssueStatus {
    match SubIssueStatus::try_from(&issue.status) {
        Ok(status) => status,
        Err(_) => unreachable!("DispatchGroup validates every Sub-issue status at construction"),
    }
}

fn is_dispatchable_status(status: &str) -> bool {
    matches!(
        status.to_ascii_lowercase().as_str(),
        "backlog" | "todo" | "triage"
    )
}

fn is_merged_status(status: &str) -> bool {
    matches!(status.to_ascii_lowercase().as_str(), "merged" | "done")
}

/// A pure Dispatch Group construction or transition error.
#[derive(Debug, Error)]
pub enum DispatchGroupError {
    /// A persisted status is not one of the compatible values.
    #[error(transparent)]
    UnknownStatus(#[from] UnknownSubIssueStatus),
    /// A requested Ticket is not part of the aggregate.
    #[error("Dispatch Group does not contain Ticket {0}")]
    UnknownTicket(String),
    /// A persisted or requested relationship violates the aggregate invariant.
    #[error("invalid Dispatch Group: {0}")]
    Invalid(String),
}

impl fmt::Display for SubIssueStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}
