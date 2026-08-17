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

const HELPER_REPOSITORY: &str = "WSG_ORCHESTRATION_REPOSITORY";
const HELPER_RESULT: &str = "WSG_ORCHESTRATION_RESULT";
const HELPER_RUNTIME_MARKER: &str = "WSG_ORCHESTRATION_RUNTIME_MARKER";

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
fn pi_orchestration_preflights_before_assignment_persistence() {
    let directory = TempDir::new().expect("temporary repository");
    fs::create_dir(directory.path().join(".jj")).expect("repository marker");
    let repository = Repository::open(directory.path()).expect("open repository");
    let parent = TicketId::parse("ENG-700").expect("Parent Ticket");
    let ticket = TicketId::parse("ENG-701").expect("Sub-issue");
    let worker = WorkerId::parse("worker-01").expect("Worker");
    install_idle_worker(&repository, &worker);
    let pool = repository.state_store().pool();
    let wsg_core::Loaded::Present(versioned) = pool.load().expect("load Pool") else {
        panic!("Pool missing")
    };
    let revision = versioned.revision().clone();
    let mut pool_state = versioned.value;
    pool_state.agent = Some(wsg_core::WireAgent::new("pi"));
    pool.commit(Expected::Match(revision), StateChange::Replace(pool_state))
        .expect("configure Pi Pool");
    let mut group_options = DispatchGroupOptions::new("gpt-5.4");
    group_options.agent = Some(wsg_core::WireAgent::new("pi"));
    group_options.provider = Some("openai".to_owned());
    let mut group = DispatchGroupState::new(
        parent.clone(),
        WireTimestamp::new("2026-08-16T10:00:00Z"),
        "owner/repo",
        group_options,
    );
    group.sub_issues.insert(
        ticket,
        SubIssueState::new("Preflight Pi", WireStatus::new("pending"), Vec::new()),
    );
    repository
        .state_store()
        .dispatch_group(parent)
        .commit(Expected::Missing, StateChange::Replace(group))
        .expect("save pending group");
    let bin = directory.path().join("isolated-bin");
    fs::create_dir(&bin).expect("fake executable directory");
    let runtime_marker = directory.path().join("runtime-started");
    fs::write(
        bin.join("pi"),
        "#!/bin/sh\n/usr/bin/touch \"$WSG_ORCHESTRATION_RUNTIME_MARKER\"\nexit 0\n",
    )
    .expect("fake Pi");
    let mut permissions = fs::metadata(bin.join("pi"))
        .expect("fake Pi metadata")
        .permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    fs::set_permissions(bin.join("pi"), permissions).expect("fake Pi permissions");
    let agent_dir = directory.path().join("empty-pi-agent");
    fs::create_dir(&agent_dir).expect("empty Pi agent directory");
    let result = directory.path().join("orchestration-result");

    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", "pi_orchestration_preflight_helper", "--ignored"])
        .env("PATH", &bin)
        .env("PI_CODING_AGENT_DIR", &agent_dir)
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_RESULT, &result)
        .env(HELPER_RUNTIME_MARKER, &runtime_marker)
        .output()
        .expect("Pi orchestration helper");

    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = fs::read_to_string(result).expect("orchestration result");
    assert!(
        result.starts_with("pending|none|idle|"),
        "unexpected result: {result}"
    );
    assert!(result.contains("pi-mcp-adapter 2.11.0"));
    assert!(
        !runtime_marker.exists(),
        "Pi started before profile preflight"
    );
}

#[test]
#[ignore]
fn pi_orchestration_preflight_helper() {
    let repository =
        Repository::open(std::env::var_os(HELPER_REPOSITORY).expect("orchestration repository"))
            .expect("open repository");
    let parent = TicketId::parse("ENG-700").expect("Parent Ticket");
    let error = repository
        .orchestration_runner()
        .advance_once(
            &OrchestrationRequest::new(parent.clone(), AgentRuntime::Claude),
            |_| {},
        )
        .expect_err("missing Pi profile should fail orchestration");
    let group = match repository
        .state_store()
        .dispatch_group(parent)
        .load()
        .expect("load group")
    {
        wsg_core::Loaded::Present(group) => group.value,
        wsg_core::Loaded::Missing => panic!("group missing"),
    };
    let issue = group.sub_issues.values().next().expect("pending issue");
    let worker = repository
        .worker_pool()
        .snapshot()
        .workers()
        .first()
        .expect("Worker")
        .clone();
    fs::write(
        std::env::var_os(HELPER_RESULT).expect("orchestration result"),
        format!(
            "{}|{}|{}|{error}",
            issue.status.as_str(),
            issue
                .worker
                .as_ref()
                .map_or("none", wsg_core::WorkerId::as_str),
            worker.status().as_str()
        ),
    )
    .expect("write orchestration result");
}

#[test]
fn persisted_runtime_matrix_survives_restart_completion_and_failure_progression() {
    for runtime in [AgentRuntime::Claude, AgentRuntime::Codex, AgentRuntime::Pi] {
        for (worker_status, retries) in [("done", 0), ("failed", 1)] {
            let directory = TempDir::new().expect("temporary repository");
            fs::create_dir(directory.path().join(".jj")).expect("repository marker");
            let repository = Repository::open(directory.path()).expect("open repository");
            let parent = TicketId::parse("ENG-800").expect("Parent Ticket");
            let ticket = TicketId::parse("ENG-801").expect("Sub-issue");
            let worker = WorkerId::parse("worker-01").expect("Worker");
            let mut options = DispatchGroupOptions::new("test-model");
            options.agent = Some(wsg_core::WireAgent::new(runtime.as_str()));
            options.provider = Some("test-provider".to_owned());
            let mut group = DispatchGroupState::new(
                parent.clone(),
                WireTimestamp::new("2026-08-17T12:00:00Z"),
                "owner/repo",
                options,
            );
            let mut issue =
                SubIssueState::new("Runtime matrix", WireStatus::new("dispatched"), Vec::new());
            issue.worker = Some(worker.clone());
            issue.retries = retries;
            issue.dispatched_at = Some(WireTimestamp::new("2026-08-17T12:01:00Z"));
            group.sub_issues.insert(ticket.clone(), issue);
            repository
                .state_store()
                .dispatch_group(parent.clone())
                .commit(Expected::Missing, StateChange::Replace(group))
                .expect("save Dispatch Group");
            let mut pool = PoolState::new(
                1,
                "owner/repo",
                vec![worker.clone()],
                WireTimestamp::new("2026-08-17T12:00:00Z"),
            );
            pool.agent = Some(wsg_core::WireAgent::new(runtime.as_str()));
            pool.provider = Some("test-provider".to_owned());
            pool.model = Some("test-model".to_owned());
            repository
                .state_store()
                .pool()
                .commit(Expected::Missing, StateChange::Replace(pool))
                .expect("save Pool");
            let mut worker_state = WorkerState::new(WireStatus::new(worker_status));
            worker_state.agent = Some(wsg_core::WireAgent::new(runtime.as_str()));
            worker_state.provider = Some("test-provider".to_owned());
            worker_state.model = Some("test-model".to_owned());
            worker_state.ticket = Some(ticket.to_string());
            worker_state.branch_name = Some("runtime-matrix".to_owned());
            worker_state.error = (worker_status == "failed").then(|| "provider failed".to_owned());
            repository
                .state_store()
                .worker(worker.clone())
                .commit(Expected::Missing, StateChange::Replace(worker_state))
                .expect("save Worker Run");

            let request_runtime = if runtime == AgentRuntime::Claude {
                AgentRuntime::Pi
            } else {
                AgentRuntime::Claude
            };
            let mut events = Vec::new();
            let summary = repository
                .orchestration_runner()
                .advance_once(
                    &OrchestrationRequest::new(parent.clone(), request_runtime),
                    |event| events.push(event),
                )
                .expect("resume terminal Run")
                .expect("terminal summary");
            assert_eq!(summary.parent(), &parent);
            assert!(events.iter().any(|event| match (worker_status, event) {
                ("done", OrchestrationEvent::Completed { ticket: actual, .. }) => {
                    actual == &ticket
                }
                ("failed", OrchestrationEvent::Failed { ticket: actual, .. }) => {
                    actual == &ticket
                }
                _ => false,
            }));
            let loaded = match repository
                .state_store()
                .dispatch_group(parent)
                .load()
                .expect("load terminal group")
            {
                wsg_core::Loaded::Present(group) => group.value,
                wsg_core::Loaded::Missing => panic!("Dispatch Group missing"),
            };
            assert_eq!(
                loaded.opts.agent.as_ref().map(wsg_core::WireAgent::as_str),
                Some(runtime.as_str())
            );
            assert_eq!(loaded.opts.provider.as_deref(), Some("test-provider"));
            assert_eq!(loaded.opts.model, "test-model");
            if worker_status == "failed" {
                let snapshot = repository.worker_pool().snapshot();
                let profile = snapshot
                    .worker(worker.as_str())
                    .expect("failed Worker")
                    .profile()
                    .expect("failed Run profile");
                assert_eq!(profile.runtime(), runtime);
                assert_eq!(
                    profile.model().and_then(AgentModel::provider),
                    Some("test-provider")
                );
                assert_eq!(profile.model().map(AgentModel::model), Some("test-model"));
            }
        }
    }
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
