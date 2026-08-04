use std::collections::VecDeque;
use std::sync::Mutex;

use wsg_core::{
    DispatchGroup, DispatchGroupBuildOptions, DispatchGroupOptions, DispatchGroupState,
    ParentTicket, SubIssueState, SubIssueStatus, TicketDiscovery, TicketId, TicketQuery,
    TicketQueryError, WireStatus, WireTimestamp,
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

fn group_from_response(response: &str) -> DispatchGroup {
    DispatchGroup::from_dependency_graph(
        &graph(response),
        DispatchGroupBuildOptions::new(
            WireTimestamp::new("2026-08-01T10:00:00Z"),
            "owner/repo",
            DispatchGroupOptions::new("opus"),
        ),
    )
    .expect("Dispatch Group")
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
fn ready_selection_returns_independent_tickets_in_ticket_order() {
    let group = group_from_response(
        r#"{"sub_issues":[{"id":"ENG-103","title":"Third","status":"Todo","blocked_by":[],"cross_repo":false},{"id":"ENG-101","title":"First","status":"Todo","blocked_by":[],"cross_repo":false},{"id":"ENG-102","title":"Second","status":"Todo","blocked_by":[],"cross_repo":false}]}"#,
    );
    assert_eq!(
        group.ready(),
        [
            TicketId::parse("ENG-101").expect("Ticket"),
            TicketId::parse("ENG-102").expect("Ticket"),
            TicketId::parse("ENG-103").expect("Ticket"),
        ]
    );
}

#[test]
fn ready_selection_waits_for_every_blocker_in_a_chain_and_diamond() {
    let chain = group_from_response(
        r#"{"sub_issues":[{"id":"ENG-101","title":"First","status":"Todo","blocked_by":[],"cross_repo":false},{"id":"ENG-102","title":"Second","status":"Todo","blocked_by":["ENG-101"],"cross_repo":false},{"id":"ENG-103","title":"Third","status":"Todo","blocked_by":["ENG-102"],"cross_repo":false}]}"#,
    );
    assert_eq!(chain.ready(), [TicketId::parse("ENG-101").expect("Ticket")]);

    let diamond = group_from_response(
        r#"{"sub_issues":[{"id":"ENG-101","title":"Root","status":"Todo","blocked_by":[],"cross_repo":false},{"id":"ENG-102","title":"Left","status":"Todo","blocked_by":["ENG-101"],"cross_repo":false},{"id":"ENG-103","title":"Right","status":"Todo","blocked_by":["ENG-101"],"cross_repo":false},{"id":"ENG-104","title":"Tip","status":"Todo","blocked_by":["ENG-102","ENG-103"],"cross_repo":false}]}"#,
    );
    assert_eq!(
        diamond.ready(),
        [TicketId::parse("ENG-101").expect("Ticket")]
    );
}

#[test]
fn ready_selection_treats_skipped_blockers_as_satisfied_but_failed_blockers_as_blocking() {
    let skipped = group_from_response(
        r#"{"sub_issues":[{"id":"ENG-101","title":"Merged","status":"Merged","blocked_by":[],"cross_repo":false},{"id":"ENG-102","title":"Ready","status":"Todo","blocked_by":["ENG-101"],"cross_repo":false}]}"#,
    );
    assert_eq!(
        skipped.ready(),
        [TicketId::parse("ENG-102").expect("Ticket")]
    );

    let mut failed_state = state();
    failed_state.sub_issues.insert(
        TicketId::parse("ENG-101").expect("Ticket"),
        SubIssueState::new("Failed", WireStatus::new("failed"), Vec::new()),
    );
    failed_state.sub_issues.insert(
        TicketId::parse("ENG-102").expect("Ticket"),
        SubIssueState::new(
            "Blocked",
            WireStatus::new("pending"),
            vec![TicketId::parse("ENG-101").expect("Ticket")],
        ),
    );
    let failed = DispatchGroup::from_state(failed_state).expect("Dispatch Group");
    assert!(failed.ready().is_empty());
}

#[test]
fn dependency_context_is_absent_without_blockers_or_when_all_bases_are_main() {
    let no_dependencies = group_from_response(
        r#"{"sub_issues":[{"id":"ENG-101","title":"Standalone","status":"Todo","blocked_by":[],"cross_repo":false}]}"#,
    );
    assert!(
        no_dependencies
            .dependency_context(&TicketId::parse("ENG-101").expect("Ticket"))
            .expect("context query")
            .is_none()
    );

    let mut main_state = state();
    let mut blocker = SubIssueState::new("Merged", WireStatus::new("skipped"), Vec::new());
    blocker.branch = Some("main".to_owned());
    main_state
        .sub_issues
        .insert(TicketId::parse("ENG-101").expect("Ticket"), blocker);
    main_state.sub_issues.insert(
        TicketId::parse("ENG-102").expect("Ticket"),
        SubIssueState::new(
            "Child",
            WireStatus::new("pending"),
            vec![TicketId::parse("ENG-101").expect("Ticket")],
        ),
    );
    let main_group = DispatchGroup::from_state(main_state).expect("main group");
    assert!(
        main_group
            .dependency_context(&TicketId::parse("ENG-102").expect("Ticket"))
            .expect("context query")
            .is_none()
    );
}

#[test]
fn dependency_context_preserves_blocker_branch_order_and_uses_the_first_as_pr_base() {
    let mut group_state = state();
    for (id, title, branch) in [
        ("ENG-101", "Auth", "adam/eng-101-auth"),
        ("ENG-102", "API", "adam/eng-102-api"),
    ] {
        let mut blocker = SubIssueState::new(title, WireStatus::new("done"), Vec::new());
        blocker.branch = Some(branch.to_owned());
        group_state
            .sub_issues
            .insert(TicketId::parse(id).expect("Ticket"), blocker);
    }
    group_state.sub_issues.insert(
        TicketId::parse("ENG-103").expect("Ticket"),
        SubIssueState::new(
            "Stacked child",
            WireStatus::new("pending"),
            vec![
                TicketId::parse("ENG-101").expect("Ticket"),
                TicketId::parse("ENG-102").expect("Ticket"),
            ],
        ),
    );
    let group = DispatchGroup::from_state(group_state).expect("Dispatch Group");
    let context = group
        .dependency_context(&TicketId::parse("ENG-103").expect("Ticket"))
        .expect("context query")
        .expect("stacked context");
    assert_eq!(
        context.base_revisions(),
        ["adam/eng-101-auth", "adam/eng-102-api"]
    );
    assert_eq!(context.pull_request_base(), "adam/eng-101-auth");
    assert!(context.description().contains("ENG-101"));
    assert!(context.description().contains("ENG-102"));
}

#[test]
fn dependency_context_rejects_a_ticket_outside_the_group() {
    let group = DispatchGroup::from_state(state()).expect("empty group");
    let error = group
        .dependency_context(&TicketId::parse("ENG-999").expect("Ticket"))
        .expect_err("unknown Ticket");
    assert!(error.to_string().contains("ENG-999"));
}

#[test]
fn empty_and_mixed_groups_report_terminal_state_and_status_counts() {
    let empty = DispatchGroup::from_state(state()).expect("empty group");
    assert!(empty.is_terminal());
    assert_eq!(empty.status_counts().done(), 0);
    assert_eq!(empty.status_counts().failed(), 0);
    assert_eq!(empty.status_counts().skipped(), 0);

    let mut mixed_state = state();
    for (id, status) in [
        ("ENG-101", "done"),
        ("ENG-102", "failed"),
        ("ENG-103", "skipped"),
        ("ENG-104", "pending"),
    ] {
        mixed_state.sub_issues.insert(
            TicketId::parse(id).expect("Ticket"),
            SubIssueState::new(id, WireStatus::new(status), Vec::new()),
        );
    }
    let mixed = DispatchGroup::from_state(mixed_state).expect("mixed group");
    assert!(!mixed.is_terminal());
    assert_eq!(mixed.status_counts().done(), 1);
    assert_eq!(mixed.status_counts().failed(), 1);
    assert_eq!(mixed.status_counts().skipped(), 1);
}

#[test]
fn maximum_wave_size_is_zero_for_an_empty_group_and_width_for_independent_tickets() {
    assert_eq!(
        DispatchGroup::from_state(state())
            .expect("empty group")
            .maximum_wave_size(),
        0
    );
    let independent = group_from_response(
        r#"{"sub_issues":[{"id":"ENG-101","title":"First","status":"Todo","blocked_by":[],"cross_repo":false},{"id":"ENG-102","title":"Second","status":"Todo","blocked_by":[],"cross_repo":false},{"id":"ENG-103","title":"Third","status":"Todo","blocked_by":[],"cross_repo":false}]}"#,
    );
    assert_eq!(independent.maximum_wave_size(), 3);
}

#[test]
fn maximum_wave_size_is_one_for_a_chain_and_two_for_a_diamond() {
    let chain = group_from_response(
        r#"{"sub_issues":[{"id":"ENG-101","title":"First","status":"Todo","blocked_by":[],"cross_repo":false},{"id":"ENG-102","title":"Second","status":"Todo","blocked_by":["ENG-101"],"cross_repo":false},{"id":"ENG-103","title":"Third","status":"Todo","blocked_by":["ENG-102"],"cross_repo":false}]}"#,
    );
    assert_eq!(chain.maximum_wave_size(), 1);

    let diamond = group_from_response(
        r#"{"sub_issues":[{"id":"ENG-101","title":"Root","status":"Todo","blocked_by":[],"cross_repo":false},{"id":"ENG-102","title":"Left","status":"Todo","blocked_by":["ENG-101"],"cross_repo":false},{"id":"ENG-103","title":"Right","status":"Todo","blocked_by":["ENG-101"],"cross_repo":false},{"id":"ENG-104","title":"Tip","status":"Todo","blocked_by":["ENG-102","ENG-103"],"cross_repo":false}]}"#,
    );
    assert_eq!(diamond.maximum_wave_size(), 2);
}

#[test]
fn maximum_wave_size_excludes_already_skipped_nodes() {
    let group = group_from_response(
        r#"{"sub_issues":[{"id":"ENG-101","title":"Merged","status":"Merged","blocked_by":[],"cross_repo":false},{"id":"ENG-102","title":"Left","status":"Todo","blocked_by":["ENG-101"],"cross_repo":false},{"id":"ENG-103","title":"Right","status":"Todo","blocked_by":["ENG-101"],"cross_repo":false}]}"#,
    );
    assert_eq!(group.maximum_wave_size(), 2);
}

#[test]
fn aggregate_wraps_valid_wire_state_without_persistence() {
    let group = DispatchGroup::from_state(state()).expect("valid state");
    assert_eq!(group.state().parent.as_str(), "ENG-100");
}
