use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

use wsg_core::{
    CommitOutcome, DispatchGroup, DispatchGroupBuildOptions, DispatchGroupEvent,
    DispatchGroupOptions, DispatchGroupState, DispatchGroupTransition, Expected, Loaded,
    ParentTicket, Repository, StateChange, SubIssueState, SubIssueStatus, TicketDiscovery,
    TicketId, TicketQuery, TicketQueryError, WireStatus, WireTimestamp, WorkerId,
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

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/compatibility");

fn fixture(name: &str) -> Vec<u8> {
    fs::read(Path::new(FIXTURES).join(name)).expect("fixture")
}

fn repository() -> (tempfile::TempDir, Repository) {
    let temp = tempfile::tempdir().expect("temp repository");
    fs::create_dir(temp.path().join(".jj")).expect("repository marker");
    let repository = Repository::open(temp.path()).expect("repository");
    (temp, repository)
}

struct FakeEventWorld {
    group: DispatchGroup,
    transitions: Vec<DispatchGroupTransition>,
}

impl FakeEventWorld {
    fn new(group: DispatchGroup) -> Self {
        Self {
            group,
            transitions: Vec::new(),
        }
    }

    fn from_state(state: DispatchGroupState) -> Self {
        Self::new(DispatchGroup::from_state(state).expect("restart state"))
    }

    fn apply(&mut self, event: DispatchGroupEvent) {
        let transition = self.group.apply(event).expect("scenario event");
        self.transitions.push(transition);
    }
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
fn every_go_dispatch_group_fixture_loads_and_round_trips_through_the_aggregate() {
    for name in [
        "dispatch-pending.json",
        "dispatch-dispatched.json",
        "dispatch-done.json",
        "dispatch-failed.json",
        "dispatch-skipped.json",
    ] {
        let source: serde_json::Value =
            serde_json::from_slice(&fixture(name)).expect("source JSON");
        let state: DispatchGroupState = serde_json::from_value(source.clone()).expect("wire state");
        let round_tripped = serde_json::to_value(
            DispatchGroup::from_state(state)
                .expect("compatible aggregate")
                .into_state(),
        )
        .expect("round-trip JSON");
        assert_eq!(round_tripped, source, "fixture {name}");
    }
}

#[test]
fn fake_event_world_drives_a_dependency_chain_across_a_restart() {
    let group = group_from_response(
        r#"{"sub_issues":[{"id":"ENG-101","title":"Foundation","status":"Todo","blocked_by":[],"cross_repo":false},{"id":"ENG-102","title":"Consumer","status":"Todo","blocked_by":["ENG-101"],"cross_repo":false}]}"#,
    );
    let mut world = FakeEventWorld::new(group);
    let first = TicketId::parse("ENG-101").expect("Ticket");
    let second = TicketId::parse("ENG-102").expect("Ticket");
    let worker_one = WorkerId::parse("worker-01").expect("Worker");
    let worker_two = WorkerId::parse("worker-02").expect("Worker");
    world.apply(DispatchGroupEvent::Dispatched {
        ticket: first.clone(),
        worker: worker_one.clone(),
        at: WireTimestamp::new("2026-08-01T10:01:00Z"),
    });
    world.apply(DispatchGroupEvent::Completed {
        ticket: first,
        worker: worker_one,
        branch: Some("adam/eng-101-foundation".to_owned()),
        at: WireTimestamp::new("2026-08-01T10:05:00Z"),
    });
    assert_eq!(
        world.group.ready().as_slice(),
        std::slice::from_ref(&second)
    );

    let resumed_state = world.group.clone().into_state();
    let mut resumed = FakeEventWorld::from_state(resumed_state);
    resumed.apply(DispatchGroupEvent::Dispatched {
        ticket: second.clone(),
        worker: worker_two.clone(),
        at: WireTimestamp::new("2026-08-01T10:06:00Z"),
    });
    resumed.apply(DispatchGroupEvent::Completed {
        ticket: second.clone(),
        worker: worker_two,
        branch: Some("adam/eng-102-consumer".to_owned()),
        at: WireTimestamp::new("2026-08-01T10:10:00Z"),
    });
    assert!(resumed.group.is_terminal());
    assert_eq!(resumed.transitions.len(), 2);
    assert_eq!(resumed.group.status_counts().done(), 2);
}

#[test]
fn aggregate_round_trip_preserves_unknown_group_and_sub_issue_fields() {
    let mut source: serde_json::Value =
        serde_json::from_slice(&fixture("dispatch-pending.json")).expect("source JSON");
    source["future_group"] = serde_json::json!({"enabled": true});
    source["sub_issues"]["ENG-101"]["future_child"] = serde_json::json!("kept");
    let state: DispatchGroupState = serde_json::from_value(source.clone()).expect("wire state");
    let round_tripped = serde_json::to_value(
        DispatchGroup::from_state(state)
            .expect("aggregate")
            .into_state(),
    )
    .expect("round-trip JSON");
    assert_eq!(round_tripped["future_group"]["enabled"], true);
    assert_eq!(
        round_tripped["sub_issues"]["ENG-101"]["future_child"],
        "kept"
    );
}

#[test]
fn aggregate_round_trip_can_be_committed_and_loaded_by_the_compatible_repository() {
    let (_temp, repository) = repository();
    let parent = TicketId::parse("ENG-100").expect("Ticket");
    let source: DispatchGroupState =
        serde_json::from_slice(&fixture("dispatch-pending.json")).expect("wire state");
    let aggregate = DispatchGroup::from_state(source).expect("aggregate");
    let store = repository.state_store().dispatch_group(parent.clone());
    assert!(matches!(
        store
            .commit(
                Expected::Missing,
                StateChange::Replace(aggregate.into_state())
            )
            .expect("commit"),
        CommitOutcome::Applied(_)
    ));
    let loaded = match store.load().expect("load") {
        Loaded::Present(versioned) => versioned.value,
        Loaded::Missing => panic!("Dispatch Group disappeared"),
    };
    DispatchGroup::from_state(loaded).expect("loaded aggregate");
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
fn lifecycle_events_dispatch_and_complete_a_ticket_with_a_branch() {
    let mut group = group_from_response(
        r#"{"sub_issues":[{"id":"ENG-101","title":"Build","status":"Todo","blocked_by":[],"cross_repo":false}]}"#,
    );
    let ticket = TicketId::parse("ENG-101").expect("Ticket");
    let worker = WorkerId::parse("worker-01").expect("Worker");
    assert_eq!(
        group
            .apply(DispatchGroupEvent::Dispatched {
                ticket: ticket.clone(),
                worker: worker.clone(),
                at: WireTimestamp::new("2026-08-01T10:01:00Z"),
            })
            .expect("dispatch"),
        DispatchGroupTransition::Dispatched
    );
    assert_eq!(
        group
            .apply(DispatchGroupEvent::Completed {
                ticket: ticket.clone(),
                worker,
                branch: Some("adam/eng-101-build".to_owned()),
                at: WireTimestamp::new("2026-08-01T10:05:00Z"),
            })
            .expect("completion"),
        DispatchGroupTransition::Completed
    );
    let state = group.state();
    let issue = &state.sub_issues[&ticket];
    assert_eq!(issue.status.as_str(), "done");
    assert_eq!(issue.branch.as_deref(), Some("adam/eng-101-build"));
    assert_eq!(
        issue.completed_at.as_ref().map(WireTimestamp::as_str),
        Some("2026-08-01T10:05:00Z")
    );
}

#[test]
fn first_failure_requires_reset_before_retry_and_second_failure_is_terminal() {
    let mut group = group_from_response(
        r#"{"sub_issues":[{"id":"ENG-101","title":"Build","status":"Todo","blocked_by":[],"cross_repo":false}]}"#,
    );
    let ticket = TicketId::parse("ENG-101").expect("Ticket");
    let worker = WorkerId::parse("worker-01").expect("Worker");
    let dispatch = || DispatchGroupEvent::Dispatched {
        ticket: ticket.clone(),
        worker: worker.clone(),
        at: WireTimestamp::new("2026-08-01T10:01:00Z"),
    };
    group.apply(dispatch()).expect("first dispatch");
    assert_eq!(
        group
            .apply(DispatchGroupEvent::Failed {
                ticket: ticket.clone(),
                worker: worker.clone(),
                at: WireTimestamp::new("2026-08-01T10:02:00Z"),
            })
            .expect("first failure"),
        DispatchGroupTransition::RetryRequired
    );
    assert_eq!(
        group.state().sub_issues[&ticket].status.as_str(),
        "dispatched"
    );
    group
        .apply(DispatchGroupEvent::Retried {
            ticket: ticket.clone(),
            worker: worker.clone(),
        })
        .expect("retry after reset");
    assert_eq!(group.state().sub_issues[&ticket].retries, 1);
    group.apply(dispatch()).expect("second dispatch");
    assert_eq!(
        group
            .apply(DispatchGroupEvent::Failed {
                ticket: ticket.clone(),
                worker,
                at: WireTimestamp::new("2026-08-01T10:04:00Z"),
            })
            .expect("second failure"),
        DispatchGroupTransition::Failed
    );
    assert_eq!(group.state().sub_issues[&ticket].status.as_str(), "failed");
    assert!(group.is_terminal());
}

#[test]
fn construction_rejects_cycles_and_impossible_persisted_relationships() {
    let cyclic = graph(
        r#"{"sub_issues":[{"id":"ENG-101","title":"A","status":"Todo","blocked_by":["ENG-102"],"cross_repo":false},{"id":"ENG-102","title":"B","status":"Todo","blocked_by":["ENG-101"],"cross_repo":false}]}"#,
    );
    assert!(
        DispatchGroup::from_dependency_graph(
            &cyclic,
            DispatchGroupBuildOptions::new(
                WireTimestamp::new("2026-08-01T10:00:00Z"),
                "owner/repo",
                DispatchGroupOptions::new("opus"),
            ),
        )
        .is_err()
    );

    let mut duplicate = state();
    duplicate.sub_issues.insert(
        TicketId::parse("ENG-101").expect("Ticket"),
        SubIssueState::new(
            "Duplicate blockers",
            WireStatus::new("pending"),
            vec![
                TicketId::parse("ENG-102").expect("Ticket"),
                TicketId::parse("ENG-102").expect("Ticket"),
            ],
        ),
    );
    duplicate.sub_issues.insert(
        TicketId::parse("ENG-102").expect("Ticket"),
        SubIssueState::new("Blocker", WireStatus::new("done"), Vec::new()),
    );
    assert!(DispatchGroup::from_state(duplicate).is_err());

    let mut unknown = state();
    unknown.sub_issues.insert(
        TicketId::parse("ENG-101").expect("Ticket"),
        SubIssueState::new(
            "Unknown blocker",
            WireStatus::new("pending"),
            vec![TicketId::parse("ENG-999").expect("Ticket")],
        ),
    );
    assert!(DispatchGroup::from_state(unknown).is_err());
}

#[test]
fn construction_rejects_duplicate_active_workers_and_excessive_retries() {
    let mut invalid = state();
    for id in ["ENG-101", "ENG-102"] {
        let mut issue = SubIssueState::new(id, WireStatus::new("dispatched"), Vec::new());
        issue.worker = Some(WorkerId::parse("worker-01").expect("Worker"));
        issue.dispatched_at = Some(WireTimestamp::new("2026-08-01T10:01:00Z"));
        invalid
            .sub_issues
            .insert(TicketId::parse(id).expect("Ticket"), issue);
    }
    assert!(DispatchGroup::from_state(invalid).is_err());

    let mut retries = state();
    let mut issue = SubIssueState::new("Too many retries", WireStatus::new("failed"), Vec::new());
    issue.retries = 2;
    retries
        .sub_issues
        .insert(TicketId::parse("ENG-101").expect("Ticket"), issue);
    assert!(DispatchGroup::from_state(retries).is_err());
}

#[test]
fn merged_event_uses_the_compatible_skipped_on_main_representation() {
    let mut group = group_from_response(
        r#"{"sub_issues":[{"id":"ENG-101","title":"Already delivered","status":"Todo","blocked_by":[],"cross_repo":false}]}"#,
    );
    let ticket = TicketId::parse("ENG-101").expect("Ticket");
    assert_eq!(
        group
            .apply(DispatchGroupEvent::Merged {
                ticket: ticket.clone(),
                at: WireTimestamp::new("2026-08-01T10:06:00Z"),
            })
            .expect("merge"),
        DispatchGroupTransition::Merged
    );
    let issue = &group.state().sub_issues[&ticket];
    assert_eq!(issue.status.as_str(), "skipped");
    assert_eq!(issue.branch.as_deref(), Some("main"));
    assert_eq!(issue.skip_reason.as_deref(), Some("merged"));
}

#[test]
fn lifecycle_events_reject_a_worker_that_does_not_own_the_ticket() {
    let mut group = group_from_response(
        r#"{"sub_issues":[{"id":"ENG-101","title":"Build","status":"Todo","blocked_by":[],"cross_repo":false}]}"#,
    );
    let ticket = TicketId::parse("ENG-101").expect("Ticket");
    group
        .apply(DispatchGroupEvent::Dispatched {
            ticket: ticket.clone(),
            worker: WorkerId::parse("worker-01").expect("Worker"),
            at: WireTimestamp::new("2026-08-01T10:01:00Z"),
        })
        .expect("dispatch");
    assert!(
        group
            .apply(DispatchGroupEvent::Completed {
                ticket,
                worker: WorkerId::parse("worker-02").expect("Worker"),
                branch: None,
                at: WireTimestamp::new("2026-08-01T10:02:00Z"),
            })
            .is_err()
    );
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
