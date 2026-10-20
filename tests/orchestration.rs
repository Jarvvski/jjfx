use std::fs::{self, OpenOptions};
use std::process::Command;

use rustix::fs::{FlockOperation, flock};

use jjfx::engine::{
    AgentModel, AgentRuntime, CommitOutcome, DispatchGroupOptions, DispatchGroupState, Expected,
    LocalMarkdownQuery, OrchestrationEvent, OrchestrationOptions, OrchestrationRequest,
    ParentTicket, PoolState, Repository, RepositoryIdentity, StateChange, SubIssueState,
    TicketDiscovery, TicketId, TicketQuery, TicketQueryError, TicketQueryRequest, WireStatus,
    WireTimestamp, WorkerId, WorkerState,
};
use tempfile::TempDir;

struct StaticQuery(&'static str);

impl TicketQuery for StaticQuery {
    fn query(&self, _request: &TicketQueryRequest) -> Result<String, TicketQueryError> {
        Ok(self.0.to_owned())
    }
}

/// Spawns a live process that leads its own process group, so a recorded PID
/// built from it is a live Run under the group-based liveness probe.
fn live_group_leader() -> std::process::Child {
    use std::os::unix::process::CommandExt;

    Command::new("sleep")
        .arg("30")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .expect("live group leader helper")
}

/// Writes one local markdown Ticket under `<root>/.scratch/<feature>/issues/`.
fn write_local_ticket(root: &std::path::Path, feature: &str, file: &str, content: &str) {
    let directory = root.join(".scratch").join(feature).join("issues");
    fs::create_dir_all(&directory).expect("issues directory");
    fs::write(directory.join(file), content).expect("Ticket file");
}

fn install_idle_worker(repository: &Repository, worker: &WorkerId) {
    install_idle_worker_with_gh_repo(repository, worker, "owner/repo");
}

fn install_idle_worker_with_gh_repo(repository: &Repository, worker: &WorkerId, gh_repo: &str) {
    repository
        .state_store()
        .pool()
        .commit(
            Expected::Missing,
            StateChange::Replace(PoolState::new(
                1,
                gh_repo,
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

/// Initialises a real colocated jj repository, so `jj git remote list` answers
/// instead of erroring on a bare `.jj` marker directory.
fn init_colocated_repository(directory: &std::path::Path) {
    let output = Command::new("jj")
        .args([
            "--config",
            "signing.behavior=drop",
            "git",
            "init",
            "--colocate",
        ])
        .arg(directory)
        .output()
        .expect("jj executable");
    assert!(output.status.success(), "jj init failed: {output:?}");
}

fn add_origin(repository: &Repository) {
    let output = Command::new("jj")
        .args([
            "git",
            "remote",
            "add",
            "origin",
            "git@github.com:owner/repo.git",
        ])
        .current_dir(repository.root())
        .output()
        .expect("add origin");
    assert!(output.status.success(), "jj remote add failed: {output:?}");
}

#[test]
fn orchestration_request_and_repository_expose_the_frontend_neutral_seam() {
    let directory = TempDir::new().expect("temporary repository");
    fs::create_dir(directory.path().join(".jj")).expect("repository marker");
    let repository = Repository::open(directory.path()).expect("open repository");
    let parent = TicketId::parse("ENG-100").expect("Parent Ticket");

    let request = OrchestrationRequest::new(parent.clone(), AgentRuntime::Codex)
        .with_model(AgentModel::new("gpt-5.4").with_provider("openai"));
    let runner = repository.orchestration_runner();

    assert_eq!(request.parent(), &parent);
    assert_eq!(request.agent_runtime(), AgentRuntime::Codex);
    let model = request.model().expect("provider-aware model selection");
    assert_eq!(model.provider(), Some("openai"));
    assert_eq!(model.model(), "gpt-5.4");
    assert_eq!(runner.repository_root(), repository.root());
}

#[test]
fn progress_projects_a_persisted_group_and_reports_absence() {
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
        ticket.clone(),
        SubIssueState::new("Foundation", WireStatus::new("pending"), Vec::new()),
    );
    repository
        .state_store()
        .dispatch_group(parent.clone())
        .commit(Expected::Missing, StateChange::Replace(group))
        .expect("save group");

    let runner = repository.orchestration_runner();
    let progress = runner
        .progress(&parent)
        .expect("projection")
        .expect("persisted group");
    assert_eq!(progress.parent(), &parent);
    assert_eq!(progress.runtime(), AgentRuntime::Claude);
    assert_eq!(progress.issues().len(), 1);
    assert_eq!(progress.issues()[0].ticket(), &ticket);
    assert_eq!(progress.issues()[0].wave(), 1);
    assert_eq!(progress.ready(), &[ticket]);
    assert_eq!(progress.maximum_wave(), 1);

    let absent = TicketId::parse("ENG-999").expect("unknown Parent");
    assert!(runner.progress(&absent).expect("projection").is_none());
}

#[test]
fn a_group_with_a_failed_blocker_terminates_instead_of_polling_forever() {
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
        SubIssueState::new("Failed", WireStatus::new("failed"), Vec::new()),
    );
    group.sub_issues.insert(
        TicketId::parse("ENG-102").expect("Sub-issue"),
        SubIssueState::new(
            "Dependent",
            WireStatus::new("pending"),
            vec![TicketId::parse("ENG-101").expect("Sub-issue")],
        ),
    );
    repository
        .state_store()
        .dispatch_group(parent.clone())
        .commit(Expected::Missing, StateChange::Replace(group))
        .expect("save group");

    // A bounded cycle budget turns a liveness bug into a failure, not a hang:
    // before the fix the runner returned `PollingExhausted` for this group.
    let summary = repository
        .orchestration_runner()
        .run(
            &OrchestrationRequest::new(parent.clone(), AgentRuntime::Claude),
            &OrchestrationOptions::new()
                .with_poll_interval(std::time::Duration::ZERO)
                .with_max_cycles(3),
            |_| {},
        )
        .expect("a group with a failed blocker must reach a terminal state");

    assert_eq!(summary.parent(), &parent);
    assert_eq!(summary.counts().failed(), 1);
    assert_eq!(summary.counts().blocked(), 1);
}

/// The runner's own advance step must observe a group whose only blocker has
/// terminally failed as terminal, so `run` cannot poll it forever.
///
/// The bounded loop is the point: before the fix `advance_once` returned `None`
/// for every cycle, and the loop fails by *observation* (the group is still
/// non-terminal after the budget) rather than by a `max_cycles` timeout or a
/// hang.
#[test]
fn advance_once_observes_a_failed_blocker_group_as_terminal() {
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
        SubIssueState::new("Failed", WireStatus::new("failed"), Vec::new()),
    );
    group.sub_issues.insert(
        TicketId::parse("ENG-102").expect("Sub-issue"),
        SubIssueState::new(
            "Dependent",
            WireStatus::new("pending"),
            vec![TicketId::parse("ENG-101").expect("Sub-issue")],
        ),
    );
    repository
        .state_store()
        .dispatch_group(parent.clone())
        .commit(Expected::Missing, StateChange::Replace(group))
        .expect("save group");

    let runner = repository.orchestration_runner();
    let request = OrchestrationRequest::new(parent.clone(), AgentRuntime::Claude);
    let mut observed = None;
    for _ in 0..8 {
        if let Some(summary) = runner.advance_once(&request, |_| {}).expect("advance") {
            observed = Some(summary);
            break;
        }
    }

    let summary = observed
        .expect("a group whose only remaining work is unreachable must be observed as terminal");
    assert_eq!(summary.parent(), &parent);
    assert_eq!(summary.counts().failed(), 1);
    assert_eq!(summary.counts().blocked(), 1);
}

#[test]
fn resumed_group_repairs_a_missing_branch_from_a_ticket_bookmark() {
    let directory = TempDir::new().expect("temporary repository");
    init_colocated_repository(directory.path());
    fs::write(directory.path().join("README"), "foundation\n").expect("file");
    let output = Command::new("jj")
        .args([
            "--config",
            "signing.behavior=drop",
            "commit",
            "-m",
            "foundation",
        ])
        .current_dir(directory.path())
        .output()
        .expect("jj executable");
    assert!(output.status.success(), "jj commit failed: {output:?}");
    let output = Command::new("jj")
        .args([
            "--config",
            "signing.behavior=drop",
            "bookmark",
            "create",
            "bob/eng-101-foundation",
        ])
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
    issue.branch = Some("bob/eng-101-removed".to_owned());
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
            &jjfx::engine::OrchestrationOptions::new()
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
            && previous == "bob/eng-101-removed"
            && current == "bob/eng-101-foundation"
    ));
    assert!(matches!(&events[2], OrchestrationEvent::Terminal(_)));
    assert_eq!(summary.parent(), &parent);
    assert_eq!(summary.counts().done(), 1);
    let loaded = match store.load().expect("load repaired group") {
        jjfx::engine::Loaded::Present(value) => value.value,
        jjfx::engine::Loaded::Missing => panic!("group disappeared"),
    };
    assert_eq!(
        loaded.sub_issues[&TicketId::parse("ENG-101").expect("Ticket")]
            .branch
            .as_deref(),
        Some("bob/eng-101-foundation")
    );
}

#[test]
fn competing_parent_runner_returns_already_running_without_waiting() {
    let directory = TempDir::new().expect("temporary repository");
    fs::create_dir(directory.path().join(".jj")).expect("repository marker");
    let repository = Repository::open(directory.path()).expect("open repository");
    let parent = TicketId::parse("ENG-100").expect("Parent Ticket");
    let lock_path = directory
        .path()
        .join(".jj/pools/default/orchestrate-eng-100.lock");
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
        jjfx::engine::OrchestrationError::AlreadyRunning { parent: actual } if actual == parent
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
    assert!(matches!(result, jjfx::engine::OrchestrationStart::Group));
    let loaded = match repository
        .state_store()
        .dispatch_group(parent_id.clone())
        .load()
        .expect("load group")
    {
        jjfx::engine::Loaded::Present(value) => value.value,
        jjfx::engine::Loaded::Missing => panic!("group missing"),
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
        jjfx::engine::Loaded::Present(value) => value.value,
        jjfx::engine::Loaded::Missing => panic!("Worker missing"),
    };
    assert_eq!(worker_state.status.as_str(), "idle");
}

#[test]
fn a_local_feature_parent_discovers_its_children_as_a_group() {
    // The README's feature-as-Parent flow: `jjfx dispatch <feature>`. A local
    // Parent is a directory rather than a Ticket file, so its id has no details
    // and orchestration must fall through to the feature's Tickets in dependency
    // order instead of failing the details fetch.
    let directory = TempDir::new().expect("temporary repository");
    fs::create_dir(directory.path().join(".jj")).expect("repository marker");
    let repository = Repository::open(directory.path()).expect("open repository");
    let worker = WorkerId::parse("worker-01").expect("Worker");
    install_idle_worker(&repository, &worker);
    write_local_ticket(
        directory.path(),
        "widgets",
        "01-foundation.md",
        "# Foundation\n\nStatus: resolved\n",
    );
    write_local_ticket(
        directory.path(),
        "widgets",
        "02-dependent.md",
        "# Dependent\n\nStatus: ready-for-agent\nBlocked by: 01\n",
    );
    let discovery = TicketDiscovery::new(LocalMarkdownQuery::new(
        directory.path().join(".scratch"),
        "ready-for-agent",
        "resolved",
    ));
    let parent_id = TicketId::parse("widgets").expect("Parent Ticket");
    let parent = ParentTicket::new(parent_id.clone());

    let start = repository
        .orchestration_runner()
        .discover(
            &OrchestrationRequest::new(parent_id.clone(), AgentRuntime::Claude),
            &parent,
            &discovery,
            &RepositoryIdentity::parse("owner/repo").expect("repository identity"),
            "owner/repo",
        )
        .expect("a local feature Parent produces a group");
    assert!(matches!(start, jjfx::engine::OrchestrationStart::Group));

    let loaded = match repository
        .state_store()
        .dispatch_group(parent_id)
        .load()
        .expect("load group")
    {
        jjfx::engine::Loaded::Present(value) => value.value,
        jjfx::engine::Loaded::Missing => panic!("group missing"),
    };
    let foundation = TicketId::parse("widgets--01-foundation").expect("Foundation");
    let dependent = TicketId::parse("widgets--02-dependent").expect("Dependent");
    assert_eq!(loaded.sub_issues.len(), 2, "{:?}", loaded.sub_issues);
    assert_eq!(loaded.sub_issues[&dependent].blocked_by, vec![foundation]);
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
    assert!(matches!(
        error,
        jjfx::engine::OrchestrationError::Execution(_)
    ));
    let worker_state = match repository
        .state_store()
        .worker(worker)
        .load()
        .expect("load Worker")
    {
        jjfx::engine::Loaded::Present(value) => value.value,
        jjfx::engine::Loaded::Missing => panic!("Worker missing"),
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
    assert!(matches!(
        error,
        jjfx::engine::OrchestrationError::Execution(_)
    ));
    let worker_state = match repository
        .state_store()
        .worker(worker)
        .load()
        .expect("load Worker")
    {
        jjfx::engine::Loaded::Present(value) => value.value,
        jjfx::engine::Loaded::Missing => panic!("Worker missing"),
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
    // A recorded PID counts as a live Run only while it leads its own process
    // group, so the "live worker" this test resumes is a real group leader.
    let mut leader = live_group_leader();
    let mut worker_state = jjfx::engine::WorkerState::new(WireStatus::new("busy"));
    worker_state.ticket = Some(ticket.as_str().to_owned());
    worker_state.pid = Some(leader.id() as i64);
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
    let _ = leader.kill();
    let _ = leader.wait();
    assert!(result.is_none());
    assert!(events.is_empty());
    let loaded = match repository
        .state_store()
        .dispatch_group(parent)
        .load()
        .expect("reload group")
    {
        jjfx::engine::Loaded::Present(value) => value.value,
        jjfx::engine::Loaded::Missing => panic!("group disappeared"),
    };
    assert_eq!(loaded.sub_issues[&ticket].status.as_str(), "dispatched");
}

#[test]
fn persisted_runtime_matrix_survives_restart_completion_and_failure_progression() {
    for runtime in [
        AgentRuntime::Claude,
        AgentRuntime::Codex,
        AgentRuntime::OpenCode,
    ] {
        for (worker_status, retries) in [("done", 0), ("failed", 1)] {
            let directory = TempDir::new().expect("temporary repository");
            fs::create_dir(directory.path().join(".jj")).expect("repository marker");
            let repository = Repository::open(directory.path()).expect("open repository");
            let parent = TicketId::parse("ENG-800").expect("Parent Ticket");
            let ticket = TicketId::parse("ENG-801").expect("Sub-issue");
            let worker = WorkerId::parse("worker-01").expect("Worker");
            let mut options = DispatchGroupOptions::new("test-model");
            options.agent = Some(jjfx::engine::WireAgent::new(runtime.as_str()));
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
            pool.agent = Some(jjfx::engine::WireAgent::new(runtime.as_str()));
            pool.provider = Some("test-provider".to_owned());
            pool.model = Some("test-model".to_owned());
            repository
                .state_store()
                .pool()
                .commit(Expected::Missing, StateChange::Replace(pool))
                .expect("save Pool");
            let mut worker_state = WorkerState::new(WireStatus::new(worker_status));
            worker_state.agent = Some(jjfx::engine::WireAgent::new(runtime.as_str()));
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
                AgentRuntime::Codex
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
                jjfx::engine::Loaded::Present(group) => group.value,
                jjfx::engine::Loaded::Missing => panic!("Dispatch Group missing"),
            };
            assert_eq!(
                loaded
                    .opts
                    .agent
                    .as_ref()
                    .map(jjfx::engine::WireAgent::as_str),
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
        jjfx::engine::OrchestrationError::PollingExhausted { parent: actual, cycles: 1 } if actual == parent
    ));
    assert!(events.iter().any(|event| matches!(
        event,
        OrchestrationEvent::WaitingForCapacity { ticket } if ticket.as_str() == "ENG-101"
    )));
}

/// A fresh repository has no `origin`, and a Pool created before any remote
/// existed carries an empty `gh_repo`. Orchestration must name the missing
/// remote and the command that adds it rather than failing with a format
/// complaint about an empty slug.
#[test]
fn prepare_names_the_missing_remote_instead_of_an_empty_slug_format_error() {
    let directory = TempDir::new().expect("temporary repository");
    init_colocated_repository(directory.path());
    let repository = Repository::open(directory.path()).expect("open repository");
    let worker = WorkerId::parse("worker-01").expect("Worker");
    install_idle_worker_with_gh_repo(&repository, &worker, "");
    let parent = ParentTicket::new(TicketId::parse("ENG-100").expect("Parent Ticket"));
    let discovery = TicketDiscovery::new(StaticQuery(r#"{"sub_issues":[]}"#));

    let error = repository
        .orchestration_runner()
        .prepare(
            &OrchestrationRequest::new(parent.id().clone(), AgentRuntime::Claude),
            &parent,
            &discovery,
        )
        .expect_err("a repository with no origin must fail at identity");

    let message = error.to_string();
    assert!(
        message.contains("origin"),
        "the failure must name the missing remote: {message}"
    );
    assert!(
        message.contains("jj git remote add origin"),
        "the failure must name the command that adds it: {message}"
    );
}

/// A local-path `origin` is not a GitHub remote, so orchestration must refuse
/// it by name rather than deriving a slug from its last two path segments -
/// which would name a repository that does not exist.
#[test]
fn prepare_refuses_a_non_github_origin_instead_of_guessing_a_slug() {
    let directory = TempDir::new().expect("temporary repository");
    init_colocated_repository(directory.path());
    let repository = Repository::open(directory.path()).expect("open repository");
    let worker = WorkerId::parse("worker-01").expect("Worker");
    install_idle_worker_with_gh_repo(&repository, &worker, "");

    let origin = TempDir::new().expect("origin repository");
    let origin_path = origin.path().join("dogfood-site-origin.git");
    let init = Command::new("jj")
        .args([
            "--config",
            "signing.behavior=drop",
            "git",
            "init",
            "--colocate",
        ])
        .arg(&origin_path)
        .output()
        .expect("jj executable");
    assert!(init.status.success(), "jj init origin failed: {init:?}");
    let add = Command::new("jj")
        .args([
            "git",
            "remote",
            "add",
            "origin",
            &origin_path.to_string_lossy(),
        ])
        .current_dir(repository.root())
        .output()
        .expect("add origin");
    assert!(add.status.success(), "jj remote add failed: {add:?}");

    let parent = ParentTicket::new(TicketId::parse("ENG-100").expect("Parent Ticket"));
    let discovery = TicketDiscovery::new(StaticQuery(r#"{"sub_issues":[]}"#));

    let error = repository
        .orchestration_runner()
        .prepare(
            &OrchestrationRequest::new(parent.id().clone(), AgentRuntime::Claude),
            &parent,
            &discovery,
        )
        .expect_err("a non-GitHub origin must fail at identity");

    let message = error.to_string();
    assert!(
        message.contains("origin is not a GitHub remote"),
        "the failure must name the non-GitHub origin: {message}"
    );
}

/// A Pool created before `dogfood/03` still carries the slug the old derivation
/// mined from a non-GitHub `origin`. Resolving identity must refuse that value by
/// name - the same named refusal - rather than carry it into a `gh` lookup for a
/// repository that never existed.
#[test]
fn prepare_refuses_a_legacy_fabricated_gh_repo_by_name() {
    let directory = TempDir::new().expect("temporary repository");
    init_colocated_repository(directory.path());
    let repository = Repository::open(directory.path()).expect("open repository");
    let worker = WorkerId::parse("worker-01").expect("Worker");
    // The exact state the dogfood drive left behind: the fabricated slug is
    // already persisted, and `origin` is still the filesystem path it came from.
    install_idle_worker_with_gh_repo(&repository, &worker, "personal/dogfood-site-origin");
    let origin = TempDir::new().expect("origin repository");
    add_local_origin(&repository, origin.path());

    let parent = ParentTicket::new(TicketId::parse("ENG-100").expect("Parent Ticket"));
    let discovery = TicketDiscovery::new(StaticQuery(r#"{"sub_issues":[]}"#));

    let error = repository
        .orchestration_runner()
        .prepare(
            &OrchestrationRequest::new(parent.id().clone(), AgentRuntime::Claude),
            &parent,
            &discovery,
        )
        .expect_err("a legacy fabricated gh_repo must be refused at the read");

    let message = error.to_string();
    assert!(
        message.contains("origin is not a GitHub remote"),
        "the failure must reuse the message `dogfood/03` introduced: {message}"
    );
    assert!(
        message.contains("dogfood-site-origin"),
        "the failure must name the remote it refused: {message}"
    );
}

/// A `gh_repo` that is not the fabricated value is the operator's own decision,
/// so a non-GitHub `origin` must not refuse it: a validation that rejects real
/// slugs would be worse than the bug it fixes.
#[test]
fn prepare_keeps_a_configured_gh_repo_that_is_not_the_fabricated_slug() {
    let directory = TempDir::new().expect("temporary repository");
    init_colocated_repository(directory.path());
    let repository = Repository::open(directory.path()).expect("open repository");
    let worker = WorkerId::parse("worker-01").expect("Worker");
    install_idle_worker_with_gh_repo(&repository, &worker, "owner/repo");
    let origin = TempDir::new().expect("origin repository");
    add_local_origin(&repository, origin.path());

    let parent_id = TicketId::parse("ENG-100").expect("Parent Ticket");
    let parent = ParentTicket::new(parent_id.clone());
    let discovery = TicketDiscovery::new(StaticQuery(
        r#"{"sub_issues":[{"id":"ENG-101","title":"Foundation","status":"Todo","blocked_by":[],"cross_repo":false}]}"#,
    ));

    let preparation = repository
        .orchestration_runner()
        .prepare(
            &OrchestrationRequest::new(parent_id.clone(), AgentRuntime::Claude),
            &parent,
            &discovery,
        )
        .expect("a configured gh_repo must survive a non-GitHub origin");
    assert!(matches!(
        preparation.start(),
        jjfx::engine::OrchestrationStart::Group
    ));

    let loaded = match repository
        .state_store()
        .dispatch_group(parent_id)
        .load()
        .expect("load group")
    {
        jjfx::engine::Loaded::Present(value) => value.value,
        jjfx::engine::Loaded::Missing => panic!("group missing"),
    };
    assert_eq!(loaded.gh_repo, "owner/repo");
}

/// Point `origin` at a real repository on disk - the shape the dogfood drive's
/// remote had, whose last two path segments (`personal/dogfood-site-origin`) are
/// not a GitHub identity.
fn add_local_origin(repository: &Repository, root: &std::path::Path) {
    let path = root.join("personal").join("dogfood-site-origin.git");
    fs::create_dir_all(path.parent().expect("origin parent")).expect("origin parent directory");
    let init = Command::new("jj")
        .args([
            "--config",
            "signing.behavior=drop",
            "git",
            "init",
            "--colocate",
        ])
        .arg(&path)
        .output()
        .expect("jj executable");
    assert!(init.status.success(), "jj init origin failed: {init:?}");
    let add = Command::new("jj")
        .args(["git", "remote", "add", "origin", &path.to_string_lossy()])
        .current_dir(repository.root())
        .output()
        .expect("add origin");
    assert!(add.status.success(), "jj remote add failed: {add:?}");
}

/// The same empty `gh_repo` must not block dispatch once an `origin` exists:
/// orchestration derives the identity from the remote and records it on the
/// group instead of failing on the empty slug.
#[test]
fn prepare_derives_identity_from_origin_when_the_pool_gh_repo_is_empty() {
    let directory = TempDir::new().expect("temporary repository");
    init_colocated_repository(directory.path());
    let repository = Repository::open(directory.path()).expect("open repository");
    let worker = WorkerId::parse("worker-01").expect("Worker");
    install_idle_worker_with_gh_repo(&repository, &worker, "");
    add_origin(&repository);
    let parent_id = TicketId::parse("ENG-100").expect("Parent Ticket");
    let parent = ParentTicket::new(parent_id.clone());
    let discovery = TicketDiscovery::new(StaticQuery(
        r#"{"sub_issues":[{"id":"ENG-101","title":"Foundation","status":"Todo","blocked_by":[],"cross_repo":false}]}"#,
    ));

    let preparation = repository
        .orchestration_runner()
        .prepare(
            &OrchestrationRequest::new(parent_id.clone(), AgentRuntime::Claude),
            &parent,
            &discovery,
        )
        .expect("an origin remote must supply the identity the Pool left empty");
    assert!(matches!(
        preparation.start(),
        jjfx::engine::OrchestrationStart::Group
    ));

    let loaded = match repository
        .state_store()
        .dispatch_group(parent_id)
        .load()
        .expect("load group")
    {
        jjfx::engine::Loaded::Present(value) => value.value,
        jjfx::engine::Loaded::Missing => panic!("group missing"),
    };
    assert_eq!(loaded.gh_repo, "owner/repo");
}
