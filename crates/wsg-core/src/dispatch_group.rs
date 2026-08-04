//! Pure Dispatch Group dependency progression and lifecycle rules.
//!
//! This module is deliberately separate from compatible state persistence. The
//! aggregate accepts and returns the wire state, but never reads files, clocks,
//! processes, Linear, or terminal output.

use std::fmt;

use thiserror::Error;

use crate::{
    DependencyGraph, DispatchGroupOptions, DispatchGroupState, SubIssueState, WireStatus,
    WireTimestamp,
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
        for issue in state.sub_issues.values() {
            SubIssueStatus::try_from(&issue.status)?;
        }
        Ok(Self { state })
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
