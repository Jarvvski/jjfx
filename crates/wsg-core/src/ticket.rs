//! Linear Ticket discovery and provider-neutral Dispatch inputs.

use std::collections::{BTreeMap, BTreeSet};

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

/// A canonical GitHub Repository identity used to scope Dispatch delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryIdentity(String);

impl RepositoryIdentity {
    /// Validates an `owner/name` Repository slug.
    pub fn parse(value: impl Into<String>) -> Result<Self, TicketValueError> {
        let value = value.into().trim().to_owned();
        let mut parts = value.split('/');
        if parts.next().is_none_or(str::is_empty)
            || parts.next().is_none_or(str::is_empty)
            || parts.next().is_some()
        {
            return Err(TicketValueError::InvalidRepositoryIdentity(value));
        }
        Ok(Self(value))
    }

    /// Returns the canonical `owner/name` slug.
    pub fn as_str(&self) -> &str {
        &self.0
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

    /// Discovers and validates the direct children of one Parent Ticket.
    pub fn dependency_graph(
        &self,
        parent: &ParentTicket,
        repository: &RepositoryIdentity,
    ) -> Result<DependencyGraph, TicketDiscoveryError> {
        let prompt = format!(
            "Fetch the direct children of Linear issue {} and their blockedBy relations. Determine cross_repo relative to {}. Return only JSON as {{\"sub_issues\":[{{\"id\":\"AMBA-42\",\"title\":\"Title\",\"status\":\"Todo\",\"blocked_by\":[\"AMBA-41\"],\"cross_repo\":false}}]}}.",
            parent.id(),
            repository.as_str(),
        );
        let output = self.query.query(&prompt)?;
        let payload: RawDependencyGraph = serde_json::from_str(&output)
            .map_err(|source| TicketDiscoveryError::MalformedResponse { source })?;
        let entry_count = payload.sub_issues.len();
        let mut candidates = BTreeMap::new();
        let mut diagnostics = Vec::new();
        for raw in payload.sub_issues {
            let subject = raw.id.clone();
            let id = match TicketId::parse(raw.id) {
                Ok(id) => id,
                Err(error) => {
                    diagnostics.push(DiscoveryDiagnostic::new(subject, error.to_string()));
                    continue;
                }
            };
            if &id == parent.id() {
                diagnostics.push(DiscoveryDiagnostic::new(
                    id.as_str(),
                    "parent cannot be its own child",
                ));
                continue;
            }
            if candidates.contains_key(&id) {
                diagnostics.push(DiscoveryDiagnostic::new(id.as_str(), "duplicate child"));
                continue;
            }
            let title = match TicketTitle::parse(raw.title) {
                Ok(title) => title,
                Err(error) => {
                    diagnostics.push(DiscoveryDiagnostic::new(id.as_str(), error.to_string()));
                    continue;
                }
            };
            let status = match TicketStatus::parse(raw.status) {
                Ok(status) => status,
                Err(error) => {
                    diagnostics.push(DiscoveryDiagnostic::new(id.as_str(), error.to_string()));
                    continue;
                }
            };
            candidates.insert(
                id.clone(),
                (
                    Ticket::new(id, title, status),
                    raw.blocked_by,
                    raw.cross_repo,
                ),
            );
        }
        if entry_count > 0 && candidates.is_empty() {
            return Err(TicketDiscoveryError::UnusableGraph {
                invalid_entries: diagnostics.len(),
            });
        }

        let known_children = candidates.keys().cloned().collect::<BTreeSet<_>>();
        let mut sub_issues = BTreeMap::new();
        for (id, (ticket, raw_blockers, cross_repository)) in candidates {
            let mut blockers = BTreeSet::new();
            for raw_blocker in raw_blockers {
                let blocker = match TicketId::parse(raw_blocker) {
                    Ok(blocker) => blocker,
                    Err(error) => {
                        diagnostics.push(DiscoveryDiagnostic::new(
                            id.as_str(),
                            format!("invalid Blocker: {error}"),
                        ));
                        continue;
                    }
                };
                if blocker == id {
                    diagnostics.push(DiscoveryDiagnostic::new(id.as_str(), "self-blocker"));
                } else if !known_children.contains(&blocker) {
                    diagnostics.push(DiscoveryDiagnostic::new(
                        id.as_str(),
                        format!("unknown Blocker {blocker}"),
                    ));
                } else {
                    blockers.insert(blocker);
                }
            }
            sub_issues.insert(
                id,
                DiscoveredSubIssue::new(
                    ticket,
                    blockers.into_iter().map(Blocker::new).collect(),
                    cross_repository,
                ),
            );
        }
        Ok(DependencyGraph {
            parent: parent.clone(),
            sub_issues,
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

#[derive(Debug, Deserialize)]
struct RawDependencyGraph {
    sub_issues: Vec<RawSubIssue>,
}

#[derive(Debug, Deserialize)]
struct RawSubIssue {
    id: String,
    title: String,
    status: String,
    blocked_by: Vec<String>,
    cross_repo: bool,
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
    /// Every returned child was invalid, so an empty result would be unsafe.
    #[error("Ticket query returned an unusable dependency graph ({invalid_entries} invalid entries)")]
    UnusableGraph {
        /// Number of diagnostics produced while validating the graph.
        invalid_entries: usize,
    },
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

/// A validated direct child and its sibling Blockers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredSubIssue {
    ticket: Ticket,
    blockers: Vec<Blocker>,
    cross_repository: bool,
}

impl DiscoveredSubIssue {
    fn new(ticket: Ticket, blockers: Vec<Blocker>, cross_repository: bool) -> Self {
        Self {
            ticket,
            blockers,
            cross_repository,
        }
    }

    /// Returns the typed child Ticket.
    pub fn ticket(&self) -> &Ticket {
        &self.ticket
    }

    /// Returns direct sibling Blockers.
    pub fn blockers(&self) -> &[Blocker] {
        &self.blockers
    }

    /// Returns whether the child targets another Repository.
    pub fn is_cross_repository(&self) -> bool {
        self.cross_repository
    }
}

/// A Parent Ticket's validated direct children and Dependencies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyGraph {
    parent: ParentTicket,
    sub_issues: BTreeMap<TicketId, DiscoveredSubIssue>,
    diagnostics: Vec<DiscoveryDiagnostic>,
}

impl DependencyGraph {
    /// Returns the Parent Ticket whose children were discovered.
    pub fn parent(&self) -> &ParentTicket {
        &self.parent
    }

    /// Returns all direct children keyed by Ticket identifier.
    pub fn sub_issues(&self) -> &BTreeMap<TicketId, DiscoveredSubIssue> {
        &self.sub_issues
    }

    /// Returns one direct child by Ticket identifier.
    pub fn sub_issue(&self, id: &TicketId) -> Option<&DiscoveredSubIssue> {
        self.sub_issues.get(id)
    }

    /// Returns diagnostics for safely excluded or repaired relationships.
    pub fn diagnostics(&self) -> &[DiscoveryDiagnostic] {
        &self.diagnostics
    }
}

/// Invalid provider-neutral Ticket data.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TicketValueError {
    /// Repository identity was not an `owner/name` slug.
    #[error("Repository identity {0:?} must use owner/name format")]
    InvalidRepositoryIdentity(String),
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
