use std::fs;
use std::process::Command;

use tempfile::TempDir;
use wsg_core::{
    AgentRuntime, CommitOutcome, DispatchGroupOptions, DispatchGroupState, Expected,
    OrchestrationEvent, OrchestrationRequest, Repository, StateChange, SubIssueState, TicketId,
    WireStatus, WireTimestamp,
};

#[test]
fn orchestration_request_and_repository_expose_the_frontend_neutral_seam() {
    let directory = TempDir::new().expect("temporary repository");
    fs::create_dir(directory.path().join(".jj")).expect("repository marker");
    let repository = Repository::open(directory.path()).expect("open repository");
    let parent = TicketId::parse("ENG-100").expect("Parent Ticket");

    let request =
        OrchestrationRequest::new(parent.clone(), AgentRuntime::Codex).with_model("gpt-5");
    let runner = repository.orchestration_runner();

    assert_eq!(request.parent(), &parent);
    assert_eq!(request.agent_runtime(), AgentRuntime::Codex);
    assert_eq!(request.model(), Some("gpt-5"));
    assert_eq!(runner.repository_root(), repository.root());
}

#[test]
fn resumed_group_repairs_a_missing_branch_from_a_ticket_bookmark() {
    let directory = TempDir::new().expect("temporary repository");
    let output = Command::new("jj")
        .args(["git", "init", "--colocate"])
        .arg(directory.path())
        .output()
        .expect("jj executable");
    assert!(output.status.success(), "jj init failed: {output:?}");
    fs::write(directory.path().join("README"), "foundation\n").expect("file");
    let output = Command::new("jj")
        .args(["commit", "-m", "foundation"])
        .current_dir(directory.path())
        .output()
        .expect("jj executable");
    assert!(output.status.success(), "jj commit failed: {output:?}");
    let output = Command::new("jj")
        .args(["bookmark", "create", "adam/eng-101-foundation"])
        .current_dir(directory.path())
        .output()
        .expect("jj executable");
    assert!(output.status.success(), "jj bookmark failed: {output:?}");

    let repository = Repository::open(directory.path()).expect("open repository");
    let parent = TicketId::parse("ENG-100").expect("Parent Ticket");
    let ticket = TicketId::parse("ENG-101").expect("Sub-issue");
    let mut state = DispatchGroupState::new(
        parent.clone(),
        WireTimestamp::new("2026-08-04T12:00:00Z"),
        "owner/repo",
        DispatchGroupOptions::new(""),
    );
    let mut issue = SubIssueState::new("Foundation", WireStatus::new("done"), Vec::new());
    issue.branch = Some("adam/eng-101-removed".to_owned());
    state.sub_issues.insert(ticket.clone(), issue);
    let store = repository.state_store().dispatch_group(parent.clone());
    assert!(matches!(
        store.commit(Expected::Missing, StateChange::Replace(state)),
        Ok(CommitOutcome::Applied(_))
    ));

    let mut events = Vec::new();
    let summary = repository
        .orchestration_runner()
        .run(
            &OrchestrationRequest::new(parent.clone(), AgentRuntime::Claude),
            &wsg_core::OrchestrationOptions::new()
                .with_poll_interval(std::time::Duration::ZERO)
                .with_max_cycles(1),
            |event| events.push(event),
        )
        .expect("resume group");

    assert_eq!(events.len(), 3);
    assert!(matches!(
        &events[0],
        OrchestrationEvent::Started { parent: actual, resumed: true } if actual == &parent
    ));
    assert!(matches!(
        &events[1],
        OrchestrationEvent::BranchRevalidated {
            ticket: actual,
            previous,
            current,
        } if actual.as_str() == "ENG-101"
            && previous == "adam/eng-101-removed"
            && current == "adam/eng-101-foundation"
    ));
    assert!(matches!(&events[2], OrchestrationEvent::Terminal(_)));
    assert_eq!(summary.parent(), &parent);
    assert_eq!(summary.counts().done(), 1);
    let loaded = match store.load().expect("load repaired group") {
        wsg_core::Loaded::Present(value) => value.value,
        wsg_core::Loaded::Missing => panic!("group disappeared"),
    };
    assert_eq!(
        loaded.sub_issues[&TicketId::parse("ENG-101").expect("Ticket")]
            .branch
            .as_deref(),
        Some("adam/eng-101-foundation")
    );
}
