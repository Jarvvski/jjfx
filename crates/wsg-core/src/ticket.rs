//! Linear Ticket discovery and provider-neutral Dispatch inputs.

use thiserror::Error;

use crate::TicketId;

/// A non-empty human-facing Ticket title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketTitle(String);

impl TicketTitle {
    /// Validates and normalizes a title returned by Linear discovery.
    pub fn parse(value: impl Into<String>) -> Result<Self, TicketValueError> {
        let value = value.into().trim().to_owned();
        if value.is_empty() {
            return Err(TicketValueError::BlankTitle);
        }
        Ok(Self(value))
    }

    /// Returns the normalized title.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A non-empty, forward-compatible Linear workflow status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketStatus(String);

impl TicketStatus {
    /// Validates and normalizes a status returned by Linear discovery.
    pub fn parse(value: impl Into<String>) -> Result<Self, TicketValueError> {
        let value = value.into().trim().to_owned();
        if value.is_empty() {
            return Err(TicketValueError::BlankStatus);
        }
        Ok(Self(value))
    }

    /// Returns the normalized workflow status.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A typed Linear work item selected for Workspace Dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    id: TicketId,
    title: TicketTitle,
    status: TicketStatus,
}

impl Ticket {
    /// Creates a Ticket from validated discovery values.
    pub fn new(id: TicketId, title: TicketTitle, status: TicketStatus) -> Self {
        Self { id, title, status }
    }

    /// Returns the stable Linear identifier.
    pub fn id(&self) -> &TicketId {
        &self.id
    }

    /// Returns the human-facing title.
    pub fn title(&self) -> &TicketTitle {
        &self.title
    }

    /// Returns the Linear workflow status.
    pub fn status(&self) -> &TicketStatus {
        &self.status
    }
}

/// A Ticket whose direct children may form a Dispatch Group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentTicket(TicketId);

impl ParentTicket {
    /// Marks a Ticket identifier as the parent being discovered.
    pub fn new(id: TicketId) -> Self {
        Self(id)
    }

    /// Returns the Parent Ticket identifier.
    pub fn id(&self) -> &TicketId {
        &self.0
    }
}

/// A sibling Sub-issue that must complete before another Sub-issue.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Blocker(TicketId);

impl Blocker {
    /// Marks a Ticket identifier as a Blocker.
    pub fn new(id: TicketId) -> Self {
        Self(id)
    }

    /// Returns the Blocker's Ticket identifier.
    pub fn id(&self) -> &TicketId {
        &self.0
    }
}

/// Invalid provider-neutral Ticket data.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TicketValueError {
    /// Linear returned no usable title.
    #[error("Ticket title cannot be blank")]
    BlankTitle,
    /// Linear returned no usable workflow status.
    #[error("Ticket status cannot be blank")]
    BlankStatus,
}
