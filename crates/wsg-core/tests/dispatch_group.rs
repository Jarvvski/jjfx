use std::collections::VecDeque;
use std::sync::Mutex;

use wsg_core::{
    DispatchGroup, DispatchGroupBuildOptions, DispatchGroupOptions, DispatchGroupState,
    ParentTicket, SubIssueStatus, TicketDiscovery, TicketId, TicketQuery, TicketQueryError,
    WireStatus, WireTimestamp,
};

struct StubQuery(Mutex<VecDeque<String>>);

impl TicketQuery for StubQuery {
    fn query(&self, _prompt: &str) -> Result<String, TicketQueryError> {
        self.0
            .lock()
            .expect("query responses")
            .pop_front()
            .ok_or_else(|| TicketQueryError::permanent("missing response"))
    }
}

fn graph(response: &str) -> wsg_core::DependencyGraph {
    let discovery =
        TicketDiscovery::new(StubQuery(Mutex::new(VecDeque::from([response.to_owned()]))));
    discovery
        .dependency_graph(
            &ParentTicket::new(TicketId::parse("ENG-100").expect("parent Ticket ID")),
            &wsg_core::RepositoryIdentity::parse("owner/repo").expect("repository identity"),
        )
        .expect("dependency graph")
}

fn state() -> DispatchGroupState {
    DispatchGroupState::new(
        TicketId::parse("ENG-100").expect("parent Ticket ID"),
        WireTimestamp::new("2026-08-01T10:00:00Z"),
        "owner/repo",
        DispatchGroupOptions::new("model"),
    )
}

#[test]
fn compatible_sub_issue_statuses_round_trip_their_wire_spellings() {
    for spelling in ["pending", "dispatched", "done", "failed", "skipped"] {
        let wire = WireStatus::new(spelling);
        let status = SubIssueStatus::try_from(&wire).expect("compatible status");
        assert_eq!(status.to_string(), spelling);
        assert_eq!(WireStatus::from(status), wire);
    }
}

#[test]
fn unknown_sub_issue_status_is_rejected_without_changing_the_wire_value() {
    let wire = WireStatus::new("future-status");
    let error = SubIssueStatus::try_from(&wire).expect_err("unknown status");
    assert_eq!(error.as_str(), "future-status");
}

#[test]
fn dependency_graph_builds_dispatchable_and_previously_delivered_sub_issues() {
    let discovered = graph(
        r#"{"sub_issues":[{"id":"ENG-101","title":"Ready","status":"Todo","blocked_by":[],"cross_repo":false},{"id":"ENG-102","title":"Merged","status":"Merged","blocked_by":["ENG-101"],"cross_repo":false},{"id":"ENG-103","title":"Other repository","status":"Todo","blocked_by":[],"cross_repo":true},{"id":"ENG-104","title":"In progress","status":"In Progress","blocked_by":[],"cross_repo":false}]}"#,
    );
    let group = DispatchGroup::from_dependency_graph(
        &discovered,
        DispatchGroupBuildOptions::new(
            WireTimestamp::new("2026-08-01T10:00:00Z"),
            "owner/repo",
            DispatchGroupOptions::new("opus"),
        ),
    )
    .expect("Dispatch Group");
    let state = group.state();
    assert_eq!(
        state.sub_issues[&TicketId::parse("ENG-101").expect("Ticket")]
            .status
            .as_str(),
        "pending"
    );
    assert_eq!(
        state.sub_issues[&TicketId::parse("ENG-102").expect("Ticket")]
            .status
            .as_str(),
        "skipped"
    );
    assert_eq!(
        state.sub_issues[&TicketId::parse("ENG-102").expect("Ticket")]
            .branch
            .as_deref(),
        Some("main")
    );
    assert_eq!(
        state.sub_issues[&TicketId::parse("ENG-103").expect("Ticket")]
            .skip_reason
            .as_deref(),
        Some("cross-repo")
    );
    assert_eq!(
        state.sub_issues[&TicketId::parse("ENG-104").expect("Ticket")]
            .skip_reason
            .as_deref(),
        Some("In Progress")
    );
}

#[test]
fn aggregate_wraps_valid_wire_state_without_persistence() {
    let group = DispatchGroup::from_state(state()).expect("valid state");
    assert_eq!(group.state().parent.as_str(), "ENG-100");
}
