use std::fs::{self, OpenOptions};
use std::process::Command;

use rustix::fs::{FlockOperation, flock};

use tempfile::TempDir;
use wsg_core::{
    AgentModel, AgentRuntime, CommitOutcome, DispatchGroupOptions, DispatchGroupState, Expected,
    OrchestrationEvent, OrchestrationOptions, OrchestrationRequest, ParentTicket, PoolState,
    Repository, RepositoryIdentity, StateChange, SubIssueState, TicketDiscovery, TicketId,
    TicketQuery, TicketQueryError, TicketQueryRequest, WireStatus, WireTimestamp, WorkerId,
    WorkerState,
};

struct StaticQuery(&'static str);

impl TicketQuery for StaticQuery {
    fn query(&self, _request: &TicketQueryRequest) -> Result<String, TicketQueryError> {
        Ok(self.0.to_owned())
    }
}

fn install_idle_worker(repository: &Repository, worker: &WorkerId) {
    repository
        .state_store()
        .pool()
        .commit(
            Expected::Missing,
            StateChange::Replace(PoolState::new(
                1,
                "owner/repo",
                vec![worker.clone()],
                WireTimestamp::new("2026-08-04T12:00:00Z"),
            )),
        )
        .expect("save pool");
    repository
        .state_store()
        .worker(worker.clone())
        .commit(
            Expected::Missing,
            StateChange::Replace(WorkerState::new(WireStatus::new("idle"))),
        )
        .expect("save idle Worker");
}

#[test]
fn orchestration_request_and_repository_expose_the_frontend_neutral_seam() {
    let directory = TempDir::new().expect("temporary repository");
    fs::create_dir(directory.path().join(".jj")).expect("repository marker");
    let repository = Repository::open(directory.path()).expect("open repository");
    let parent = TicketId::parse("ENG-100").expect("Parent Ticket");

    let request = OrchestrationRequest::new(parent.clone(), AgentRuntime::Pi)
        .with_model(AgentModel::new("gpt-5.4").with_provider("openai"));
    let runner = repository.orchestration_runner();

    assert_eq!(request.parent(), &parent);
    assert_eq!(request.agent_runtime(), AgentRuntime::Pi);
    let model = request.model().expect("provider-aware model selection");
    assert_eq!(model.provider(), Some("openai"));
    assert_eq!(model.model(), "gpt-5.4");
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

#[test]
fn competing_parent_runner_returns_already_running_without_waiting() {
    let directory = TempDir::new().expect("temporary repository");
    fs::create_dir(directory.path().join(".jj")).expect("repository marker");
    let repository = Repository::open(directory.path()).expect("open repository");
    let parent = TicketId::parse("ENG-100").expect("Parent Ticket");
    let lock_path = directory.path().join(".jj/pool/orchestrate-eng-100.lock");
    fs::create_dir_all(lock_path.parent().expect("lock parent")).expect("lock directory");
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .expect("lock file");
    flock(&lock_file, FlockOperation::LockExclusive).expect("hold runner lock");

    let error = repository
        .orchestration_runner()
        .advance_once(
            &OrchestrationRequest::new(parent.clone(), AgentRuntime::Claude),
            |_| {},
        )
        .expect_err("second runner must be rejected");
    assert!(matches!(
        error,
        wsg_core::OrchestrationError::AlreadyRunning { parent: actual } if actual == parent
    ));
}

#[test]
fn discovery_releases_placeholder_before_persisting_a_real_group() {
    let directory = TempDir::new().expect("temporary repository");
    fs::create_dir(directory.path().join(".jj")).expect("repository marker");
    let repository = Repository::open(directory.path()).expect("open repository");
    let worker = WorkerId::parse("worker-01").expect("Worker");
    install_idle_worker(&repository, &worker);
    let parent_id = TicketId::parse("ENG-100").expect("Parent Ticket");
    let parent = ParentTicket::new(parent_id.clone());
    let discovery = TicketDiscovery::new(StaticQuery(
        r#"{"sub_issues":[{"id":"ENG-101","title":"Foundation","status":"Todo","blocked_by":[],"cross_repo":false}]}"#,
    ));

    let result = repository
        .orchestration_runner()
        .discover(
            &OrchestrationRequest::new(parent_id.clone(), AgentRuntime::Claude),
            &parent,
            &discovery,
            &RepositoryIdentity::parse("owner/repo").expect("repository identity"),
            "owner/repo",
        )
        .expect("discover group");
    assert!(matches!(result, wsg_core::OrchestrationStart::Group));
    let loaded = match repository
        .state_store()
        .dispatch_group(parent_id.clone())
        .load()
        .expect("load group")
    {
        wsg_core::Loaded::Present(value) => value.value,
        wsg_core::Loaded::Missing => panic!("group missing"),
    };
    assert!(
        loaded
            .sub_issues
            .contains_key(&TicketId::parse("ENG-101").expect("Ticket"))
    );
    let worker_state = match repository
        .state_store()
        .worker(worker)
        .load()
        .expect("load Worker")
    {
        wsg_core::Loaded::Present(value) => value.value,
        wsg_core::Loaded::Missing => panic!("Worker missing"),
    };
    assert_eq!(worker_state.status.as_str(), "idle");
}

#[test]
fn failed_graph_discovery_releases_placeholder_and_surfaces_the_query_error() {
    let directory = TempDir::new().expect("temporary repository");
    fs::create_dir(directory.path().join(".jj")).expect("repository marker");
    let repository = Repository::open(directory.path()).expect("open repository");
    let worker = WorkerId::parse("worker-01").expect("Worker");
    install_idle_worker(&repository, &worker);
    let parent_id = TicketId::parse("ENG-100").expect("Parent Ticket");
    let parent = ParentTicket::new(parent_id.clone());
    let discovery = TicketDiscovery::new(StaticQuery("not JSON"));

    let error = repository
        .orchestration_runner()
        .discover(
            &OrchestrationRequest::new(parent_id, AgentRuntime::Claude),
            &parent,
            &discovery,
            &RepositoryIdentity::parse("owner/repo").expect("repository identity"),
            "owner/repo",
        )
        .expect_err("malformed graph should fail");
    assert!(matches!(error, wsg_core::OrchestrationError::Execution(_)));
    let worker_state = match repository
        .state_store()
        .worker(worker)
        .load()
        .expect("load Worker")
    {
        wsg_core::Loaded::Present(value) => value.value,
        wsg_core::Loaded::Missing => panic!("Worker missing"),
    };
    assert_eq!(worker_state.status.as_str(), "idle");
}

#[test]
fn empty_graph_uses_the_reserved_parent_fallback_and_releases_on_launch_failure() {
    let directory = TempDir::new().expect("temporary repository");
    fs::create_dir(directory.path().join(".jj")).expect("repository marker");
    let repository = Repository::open(directory.path()).expect("open repository");
    let worker = WorkerId::parse("worker-01").expect("Worker");
    install_idle_worker(&repository, &worker);
    let parent_id = TicketId::parse("ENG-100").expect("Parent Ticket");
    let parent = ParentTicket::new(parent_id.clone());
    let discovery = TicketDiscovery::new(StaticQuery(r#"{"sub_issues":[]}"#));

    let error = repository
        .orchestration_runner()
        .discover(
            &OrchestrationRequest::new(parent_id, AgentRuntime::Claude),
            &parent,
            &discovery,
            &RepositoryIdentity::parse("owner/repo").expect("repository identity"),
            "owner/repo",
        )
        .expect_err("fallback launch needs a provisioned Worker workspace");
    assert!(matches!(error, wsg_core::OrchestrationError::Execution(_)));
    let worker_state = match repository
        .state_store()
        .worker(worker)
        .load()
        .expect("load Worker")
    {
        wsg_core::Loaded::Present(value) => value.value,
        wsg_core::Loaded::Missing => panic!("Worker missing"),
    };
    assert_eq!(worker_state.status.as_str(), "idle");
}

#[test]
fn restart_does_not_duplicate_a_persisted_assignment_with_a_live_worker() {
    let directory = TempDir::new().expect("temporary repository");
    fs::create_dir(directory.path().join(".jj")).expect("repository marker");
    let repository = Repository::open(directory.path()).expect("open repository");
    let parent = TicketId::parse("ENG-100").expect("Parent Ticket");
    let ticket = TicketId::parse("ENG-101").expect("Sub-issue");
    let worker = WorkerId::parse("worker-02").expect("Worker");
    let mut group = DispatchGroupState::new(
        parent.clone(),
        WireTimestamp::new("2026-07-27T10:00:00Z"),
        "owner/repo",
        DispatchGroupOptions::new(""),
    );
    let mut issue = SubIssueState::new("First child", WireStatus::new("dispatched"), Vec::new());
    issue.worker = Some(worker.clone());
    issue.dispatched_at = Some(WireTimestamp::new("2026-07-27T10:01:00Z"));
    group.sub_issues.insert(ticket.clone(), issue);
    repository
        .state_store()
        .dispatch_group(parent.clone())
        .commit(Expected::Missing, StateChange::Replace(group))
        .expect("save dispatched group");
    let mut pool = PoolState::new(
        1,
        "owner/repo",
        vec![worker.clone()],
        WireTimestamp::new("2026-07-27T10:00:00Z"),
    );
    repository
        .state_store()
        .pool()
        .commit(Expected::Missing, StateChange::Replace(pool.clone()))
        .expect("save pool");
    let mut worker_state = wsg_core::WorkerState::new(WireStatus::new("busy"));
    worker_state.ticket = Some(ticket.as_str().to_owned());
    worker_state.pid = Some(std::process::id() as i64);
    repository
        .state_store()
        .worker(worker.clone())
        .commit(Expected::Missing, StateChange::Replace(worker_state))
        .expect("save busy Worker");
    pool.workers = vec![worker];

    let mut events = Vec::new();
    let result = repository
        .orchestration_runner()
        .advance_once(
            &OrchestrationRequest::new(parent.clone(), AgentRuntime::Claude),
            |event| events.push(event),
        )
        .expect("resume live assignment");
    assert!(result.is_none());
    assert!(events.is_empty());
    let loaded = match repository
        .state_store()
        .dispatch_group(parent)
        .load()
        .expect("reload group")
    {
        wsg_core::Loaded::Present(value) => value.value,
        wsg_core::Loaded::Missing => panic!("group disappeared"),
    };
    assert_eq!(loaded.sub_issues[&ticket].status.as_str(), "dispatched");
}

#[test]
fn detached_entrypoint_returns_terminal_summary_for_persisted_group() {
    let directory = TempDir::new().expect("temporary repository");
    fs::create_dir(directory.path().join(".jj")).expect("repository marker");
    let repository = Repository::open(directory.path()).expect("open repository");
    let parent = TicketId::parse("ENG-100").expect("Parent Ticket");
    let mut group = DispatchGroupState::new(
        parent.clone(),
        WireTimestamp::new("2026-08-04T12:00:00Z"),
        "owner/repo",
        DispatchGroupOptions::new(""),
    );
    group.sub_issues.insert(
        TicketId::parse("ENG-101").expect("Sub-issue"),
        SubIssueState::new("Foundation", WireStatus::new("done"), Vec::new()),
    );
    repository
        .state_store()
        .dispatch_group(parent.clone())
        .commit(Expected::Missing, StateChange::Replace(group))
        .expect("save group");

    let summary = repository
        .orchestration_runner()
        .run_detached(
            &OrchestrationRequest::new(parent.clone(), AgentRuntime::Claude),
            &OrchestrationOptions::new()
                .with_poll_interval(std::time::Duration::ZERO)
                .with_max_cycles(1),
            |_| {},
        )
        .expect("detached entrypoint");
    assert_eq!(summary.parent(), &parent);
    assert_eq!(summary.counts().done(), 1);
}

#[test]
fn bounded_polling_reports_capacity_wait_without_busy_looping() {
    let directory = TempDir::new().expect("temporary repository");
    fs::create_dir(directory.path().join(".jj")).expect("repository marker");
    let repository = Repository::open(directory.path()).expect("open repository");
    let parent = TicketId::parse("ENG-100").expect("Parent Ticket");
    let ticket = TicketId::parse("ENG-101").expect("Sub-issue");
    let mut group = DispatchGroupState::new(
        parent.clone(),
        WireTimestamp::new("2026-08-04T12:00:00Z"),
        "owner/repo",
        DispatchGroupOptions::new(""),
    );
    group.sub_issues.insert(
        ticket,
        SubIssueState::new("Foundation", WireStatus::new("pending"), Vec::new()),
    );
    repository
        .state_store()
        .dispatch_group(parent.clone())
        .commit(Expected::Missing, StateChange::Replace(group))
        .expect("save group");
    repository
        .state_store()
        .pool()
        .commit(
            Expected::Missing,
            StateChange::Replace(PoolState::new(
                0,
                "owner/repo",
                Vec::<WorkerId>::new(),
                WireTimestamp::new("2026-08-04T12:00:00Z"),
            )),
        )
        .expect("save empty pool");

    let mut events = Vec::new();
    let error = repository
        .orchestration_runner()
        .run(
            &OrchestrationRequest::new(parent.clone(), AgentRuntime::Claude),
            &OrchestrationOptions::new()
                .with_poll_interval(std::time::Duration::ZERO)
                .with_max_cycles(1),
            |event| events.push(event),
        )
        .expect_err("one bounded cycle cannot complete pending work");
    assert!(matches!(
        error,
        wsg_core::OrchestrationError::PollingExhausted { parent: actual, cycles: 1 } if actual == parent
    ));
    assert!(events.iter().any(|event| matches!(
        event,
        OrchestrationEvent::WaitingForCapacity { ticket } if ticket.as_str() == "ENG-101"
    )));
}
