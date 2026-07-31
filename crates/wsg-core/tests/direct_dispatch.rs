use std::ffi::OsStr;
use std::process::Command;

use tempfile::TempDir;
use wsg_core::{
    AgentRuntimeCapabilities, DirectDispatchExecution, DirectDispatchFailure,
    DirectDispatchFailurePhase, DirectDispatchOutcome, DirectDispatchRequest, DirectDispatchResult,
    DirectDispatchSuccess, DispatchBudget, DispatchDependencyContext, PoolCapacity, Repository,
    RunMode, Ticket, TicketId, TicketStatus, TicketTitle, WorkerId, WorkerStatus,
};

#[test]
fn direct_dispatch_resolves_delivery_identity_and_builds_dependency_aware_prompts() {
    let (_temporary_directory, repository) = local_repository();
    for arguments in [
        vec!["config", "set", "--repo", "user.email", "owner@example.com"],
        vec!["config", "set", "--repo", "user.name", "Owner Person"],
        vec![
            "git",
            "remote",
            "add",
            "origin",
            "git@github.com:owner/repo.git",
        ],
    ] {
        let output = Command::new("jj")
            .args(arguments)
            .current_dir(repository.root())
            .output()
            .expect("configure repository identity");
        assert!(
            output.status.success(),
            "repository configuration failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("grow Worker Pool");
    let dependency = DispatchDependencyContext::new(
        vec!["owner/eng-400-foundation".to_owned(), "main".to_owned()],
        "- Branch owner/eng-400-foundation implements ENG-400",
        "owner/eng-400-foundation",
    );
    let request = DirectDispatchRequest::new(
        ticket("ENG-404", "Build dependency-aware prompt"),
        RunMode::Background,
    )
    .with_model("opus")
    .with_dependency_context(dependency);
    let dispatch = repository.direct_dispatch();
    let reservation = dispatch.reserve(&request).expect("reserve Worker");

    let invocation = dispatch
        .build_invocation(&reservation, &request)
        .expect("build Direct Dispatch invocation");
    let command = reservation
        .agent_runtime()
        .command(&invocation, AgentRuntimeCapabilities::default());
    let rendered = command
        .get_args()
        .map(OsStr::to_string_lossy)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(rendered.contains("owner/repo"));
    assert!(rendered.contains("owner@example.com"));
    assert!(rendered.contains("owner/eng-404"));
    assert!(rendered.contains("STACKED BRANCH"));
    assert!(rendered.contains("owner/eng-400-foundation implements ENG-400"));
    assert!(rendered.contains("--base owner/eng-400-foundation"));
}

#[test]
fn direct_dispatch_prepares_the_worker_workspace_on_main_under_an_operation_lock() {
    let (_temporary_directory, repository) = local_repository();
    let main = Command::new("jj")
        .args(["bookmark", "create", "main", "-r", "@"])
        .current_dir(repository.root())
        .output()
        .expect("create main bookmark");
    assert!(main.status.success());
    let worker = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("grow Worker Pool")
        .added_workers()[0]
        .clone();

    let workspace = repository
        .prepare_worker_workspace(&worker, &[])
        .expect("prepare Worker Workspace");

    assert!(workspace.path().is_dir());
    assert!(
        repository
            .root()
            .join(".jj/pool")
            .join(format!("{worker}.workspace.lock"))
            .is_file()
    );
    let parent = Command::new("jj")
        .args(["log", "-r", "@-", "--no-graph", "-T", "bookmarks"])
        .current_dir(workspace.path())
        .output()
        .expect("read prepared parent");
    assert!(parent.status.success());
    assert!(String::from_utf8_lossy(&parent.stdout).contains("main"));
}

#[test]
fn named_direct_dispatch_reserves_only_the_requested_idle_worker() {
    let (_temporary_directory, repository) = local_repository();
    let workers = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(2).expect("capacity"))
        .expect("grow Worker Pool")
        .added_workers()
        .to_vec();
    let request = DirectDispatchRequest::new(
        ticket("ENG-403", "Use the selected Worker"),
        RunMode::Background,
    )
    .to_worker(workers[1].clone());

    let reservation = repository
        .direct_dispatch()
        .reserve(&request)
        .expect("reserve named Worker");

    assert_eq!(reservation.worker_id(), &workers[1]);
    let snapshot = repository.worker_pool().snapshot();
    assert_eq!(
        snapshot
            .worker(workers[0].as_str())
            .expect("first Worker")
            .status(),
        WorkerStatus::Idle
    );
    assert_eq!(
        snapshot
            .worker(workers[1].as_str())
            .expect("selected Worker")
            .ticket(),
        Some("ENG-403")
    );
}

#[test]
fn direct_dispatch_contract_carries_inputs_and_preserves_outcome_order() {
    let first = ticket("ENG-401", "Prepare dependency bases");
    let second = ticket("ENG-402", "Launch the worker");
    let dependency = DispatchDependencyContext::new(
        vec!["owner/eng-400-foundation".to_owned(), "main".to_owned()],
        "- owner/eng-400-foundation implements ENG-400",
        "owner/eng-400-foundation",
    );
    let request = DirectDispatchRequest::new(first.clone(), RunMode::Background)
        .with_model("opus")
        .with_budget(DispatchBudget::maximum_usd(15).expect("budget"))
        .with_dependency_context(dependency.clone());

    assert_eq!(request.ticket(), &first);
    assert_eq!(request.model(), Some("opus"));
    assert_eq!(request.budget(), DispatchBudget::MaximumUsd(15));
    assert_eq!(request.mode(), RunMode::Background);
    assert_eq!(request.dependency_context(), Some(&dependency));
    assert_eq!(
        request
            .dependency_context()
            .expect("dependency context")
            .base_revisions(),
        ["owner/eng-400-foundation", "main"]
    );

    let result = DirectDispatchResult::new(
        vec![
            DirectDispatchOutcome::Succeeded(DirectDispatchSuccess::new(
                first.clone(),
                WorkerId::parse("worker-01").expect("Worker ID"),
                DirectDispatchExecution::Background { pid: 42 },
            )),
            DirectDispatchOutcome::Failed(DirectDispatchFailure::new(
                second.clone(),
                Some(WorkerId::parse("worker-02").expect("Worker ID")),
                DirectDispatchFailurePhase::Workspace,
                "cannot prepare Workspace",
            )),
        ],
        false,
    );

    assert_eq!(result.outcomes()[0].ticket(), &first);
    assert_eq!(result.outcomes()[1].ticket(), &second);
    assert!(!result.is_partial());
}

fn local_repository() -> (TempDir, Repository) {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let output = Command::new("jj")
        .args(["--config", "signing.behavior=drop", "git", "init"])
        .arg(temporary_directory.path())
        .output()
        .expect("jj init");
    assert!(
        output.status.success(),
        "jj init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let repository = Repository::open(temporary_directory.path()).expect("open repository");
    (temporary_directory, repository)
}

fn ticket(id: &str, title: &str) -> Ticket {
    Ticket::new(
        TicketId::parse(id).expect("Ticket ID"),
        TicketTitle::parse(title).expect("Ticket title"),
        TicketStatus::parse("Todo").expect("Ticket status"),
    )
}
