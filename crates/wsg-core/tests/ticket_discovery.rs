use std::collections::VecDeque;
use std::sync::Mutex;

use wsg_core::{
    Blocker, ParentTicket, ReadyTicketFilter, RepositoryIdentity, Ticket, TicketDiscovery,
    TicketId, TicketQuery, TicketQueryError, TicketStatus, TicketTitle,
};

struct StubQuery {
    responses: Mutex<VecDeque<Result<String, TicketQueryError>>>,
}

impl StubQuery {
    fn returning(response: &str) -> Self {
        Self {
            responses: Mutex::new(VecDeque::from([Ok(response.to_owned())])),
        }
    }
}

impl TicketQuery for StubQuery {
    fn query(&self, _prompt: &str) -> Result<String, TicketQueryError> {
        self.responses
            .lock()
            .expect("query responses")
            .pop_front()
            .expect("a configured query response")
    }
}

#[test]
fn parent_ticket_discovery_returns_a_typed_dependency_graph() {
    let discovery = TicketDiscovery::new(StubQuery::returning(
        r#"{"sub_issues":[{"id":"AMBA-41","title":"Foundation","status":"Done","blocked_by":[],"cross_repo":false},{"id":"AMBA-42","title":"Ship typed discovery","status":"Todo","blocked_by":["AMBA-41"],"cross_repo":false}]}"#,
    ));
    let parent = ParentTicket::new(TicketId::parse("AMBA-40").expect("Parent Ticket ID"));
    let repository = RepositoryIdentity::parse("owner/repo").expect("Repository identity");

    let graph = discovery
        .dependency_graph(&parent, &repository)
        .expect("dependency graph should be discovered");

    assert_eq!(graph.parent(), &parent);
    assert_eq!(graph.sub_issues().len(), 2);
    let ticket = TicketId::parse("AMBA-42").expect("Ticket ID");
    let child = graph.sub_issue(&ticket).expect("discovered Sub-issue");
    assert_eq!(child.ticket().title().as_str(), "Ship typed discovery");
    assert_eq!(child.blockers().len(), 1);
    assert_eq!(child.blockers()[0].id().as_str(), "AMBA-41");
    assert!(!child.is_cross_repository());
    assert!(graph.diagnostics().is_empty());
}

#[test]
fn ready_ticket_discovery_returns_typed_tickets_matching_the_filter() {
    let discovery = TicketDiscovery::new(StubQuery::returning(
        r#"{"tickets":[{"id":"AMBA-42","title":"Ship typed discovery","status":"Todo"}]}"#,
    ));
    let filter = ReadyTicketFilter::new(
        "ready-for-agent",
        TicketStatus::parse("Todo").expect("expected workflow status"),
    )
    .expect("Ready Ticket filter");

    let tickets = discovery
        .ready_tickets(&filter)
        .expect("Ready Tickets should be discovered");

    assert_eq!(
        tickets.tickets(),
        &[Ticket::new(
            TicketId::parse("AMBA-42").expect("Ticket ID"),
            TicketTitle::parse("Ship typed discovery").expect("Ticket title"),
            TicketStatus::parse("Todo").expect("Ticket status"),
        )]
    );
    assert!(tickets.diagnostics().is_empty());
}

#[test]
fn ticket_values_preserve_valid_linear_identity_and_relationships() {
    let id = TicketId::parse("AMBA-42").expect("Ticket ID");
    let ticket = Ticket::new(
        id.clone(),
        TicketTitle::parse("Ship typed discovery").expect("Ticket title"),
        TicketStatus::parse("Todo").expect("Ticket status"),
    );
    let parent = ParentTicket::new(TicketId::parse("AMBA-40").expect("Parent Ticket ID"));
    let blocker = Blocker::new(TicketId::parse("AMBA-41").expect("Blocker ID"));

    assert_eq!(ticket.id(), &id);
    assert_eq!(ticket.title().as_str(), "Ship typed discovery");
    assert_eq!(ticket.status().as_str(), "Todo");
    assert_eq!(parent.id().as_str(), "AMBA-40");
    assert_eq!(blocker.id().as_str(), "AMBA-41");
}

#[test]
fn ticket_values_reject_missing_titles_and_statuses() {
    assert_eq!(
        TicketTitle::parse("   ").expect_err("blank title should fail").to_string(),
        "Ticket title cannot be blank"
    );
    assert_eq!(
        TicketStatus::parse("").expect_err("blank status should fail").to_string(),
        "Ticket status cannot be blank"
    );
}
