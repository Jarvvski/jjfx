//! Linear Ticket discovery and provider-neutral Dispatch inputs.

use serde::Deserialize;
use thiserror::Error;

use crate::TicketId;

/// Executes one short-lived, read-only query against Linear through an Agent Runtime.
pub trait TicketQuery {
    /// Returns the provider-neutral text response for `prompt`.
    fn query(&self, prompt: &str) -> Result<String, TicketQueryError>;
}

/// A failure reported by a Ticket query adapter.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct TicketQueryError {
    message: String,
}

impl TicketQueryError {
    /// Creates an adapter failure with provider context already attached.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Selects Ready Tickets by dispatch label and expected Linear workflow status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyTicketFilter {
    label: String,
    status: TicketStatus,
}

impl ReadyTicketFilter {
    /// Creates a validated Ready Ticket filter.
    pub fn new(
        label: impl Into<String>,
        status: TicketStatus,
    ) -> Result<Self, TicketValueError> {
        let label = label.into().trim().to_owned();
        if label.is_empty() {
            return Err(TicketValueError::BlankLabel);
        }
        Ok(Self { label, status })
    }
}

/// A safely excluded discovery entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryDiagnostic {
    subject: String,
    reason: String,
}

impl DiscoveryDiagnostic {
    fn new(subject: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            reason: reason.into(),
        }
    }

    /// Returns the provider-supplied identifier associated with the entry.
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// Returns why the entry was excluded or repaired.
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// Ready Tickets plus diagnostics for entries that were safely excluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyTickets {
    tickets: Vec<Ticket>,
    diagnostics: Vec<DiscoveryDiagnostic>,
}

impl ReadyTickets {
    /// Returns validated Ready Tickets in provider order.
    pub fn tickets(&self) -> &[Ticket] {
        &self.tickets
    }

    /// Returns diagnostics for excluded provider entries.
    pub fn diagnostics(&self) -> &[DiscoveryDiagnostic] {
        &self.diagnostics
    }
}

/// Discovers typed Linear Tickets behind one provider-neutral interface.
#[derive(Debug)]
pub struct TicketDiscovery<Q> {
    query: Q,
}

impl<Q> TicketDiscovery<Q>
where
    Q: TicketQuery,
{
    /// Creates discovery over a production or deterministic query adapter.
    pub fn new(query: Q) -> Self {
        Self { query }
    }

    /// Discovers Tickets carrying the configured label in the expected status.
    pub fn ready_tickets(
        &self,
        filter: &ReadyTicketFilter,
    ) -> Result<ReadyTickets, TicketDiscoveryError> {
        let prompt = format!(
            "Use Linear to find issues with label {:?} in {:?} status. Return only JSON as {{\"tickets\":[{{\"id\":\"AMBA-42\",\"title\":\"Title\",\"status\":\"{}\"}}]}}.",
            filter.label,
            filter.status.as_str(),
            filter.status.as_str(),
        );
        let output = self.query.query(&prompt)?;
        let payload: RawReadyTickets = serde_json::from_str(&output)
            .map_err(|source| TicketDiscoveryError::MalformedResponse { source })?;
        let mut tickets = Vec::with_capacity(payload.tickets.len());
        let mut diagnostics = Vec::new();
        for raw in payload.tickets {
            match ready_ticket(raw, &filter.status) {
                Ok(ticket) => tickets.push(ticket),
                Err(diagnostic) => diagnostics.push(diagnostic),
            }
        }
        Ok(ReadyTickets {
            tickets,
            diagnostics,
        })
    }
}

#[derive(Debug, Deserialize)]
struct RawReadyTickets {
    tickets: Vec<RawTicket>,
}

#[derive(Debug, Deserialize)]
struct RawTicket {
    id: String,
    title: String,
    status: String,
}

fn ready_ticket(
    raw: RawTicket,
    expected_status: &TicketStatus,
) -> Result<Ticket, DiscoveryDiagnostic> {
    let subject = raw.id.clone();
    let id = TicketId::parse(raw.id)
        .map_err(|error| DiscoveryDiagnostic::new(&subject, error.to_string()))?;
    let title = TicketTitle::parse(raw.title)
        .map_err(|error| DiscoveryDiagnostic::new(&subject, error.to_string()))?;
    let status = TicketStatus::parse(raw.status)
        .map_err(|error| DiscoveryDiagnostic::new(&subject, error.to_string()))?;
    if status != *expected_status {
        return Err(DiscoveryDiagnostic::new(
            subject,
            format!(
                "expected status {:?}, found {:?}",
                expected_status.as_str(),
                status.as_str()
            ),
        ));
    }
    Ok(Ticket::new(id, title, status))
}

/// A failure that makes a Ticket discovery result unusable as a whole.
#[derive(Debug, Error)]
pub enum TicketDiscoveryError {
    /// The selected Agent Runtime query failed.
    #[error("Ticket query failed: {0}")]
    Query(#[from] TicketQueryError),
    /// The Agent Runtime did not return the constrained response shape.
    #[error("Ticket query returned malformed JSON: {source}")]
    MalformedResponse {
        /// JSON shape or syntax failure.
        #[source]
        source: serde_json::Error,
    },
}

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
    /// Ready Ticket discovery was configured without a label.
    #[error("Ready Ticket label cannot be blank")]
    BlankLabel,
    /// Linear returned no usable title.
    #[error("Ticket title cannot be blank")]
    BlankTitle,
    /// Linear returned no usable workflow status.
    #[error("Ticket status cannot be blank")]
    BlankStatus,
}
