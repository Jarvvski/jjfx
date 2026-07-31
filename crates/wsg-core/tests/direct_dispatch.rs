use std::ffi::OsStr;
use std::fs;
use std::process::Command;

use tempfile::TempDir;
use wsg_core::{
    AgentRuntimeCapabilities, CommitOutcome, DirectDispatchError, DirectDispatchExecution,
    DirectDispatchFailure, DirectDispatchFailurePhase, DirectDispatchOutcome,
    DirectDispatchRequest, DirectDispatchResult, DirectDispatchSuccess, DispatchBudget,
    DispatchDependencyContext, Expected, Loaded, PoolCapacity, Repository, RunMode, StateChange,
    Ticket, TicketId, TicketStatus, TicketTitle, WireAgent, WorkerId, WorkerPoolError,
    WorkerStatus,
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
fn direct_dispatch_releases_only_its_reservation_after_workspace_failure() {
    let (_temporary_directory, repository) = local_repository();
    let worker = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("grow Worker Pool")
        .added_workers()[0]
        .clone();
    let workspace = repository
        .root()
        .parent()
        .expect("Repository parent")
        .join(format!(
            "{}-workspaces/{worker}",
            repository
                .root()
                .file_name()
                .expect("Repository name")
                .to_string_lossy()
        ));
    fs::remove_dir_all(workspace).expect("remove Worker Workspace");
    let request = DirectDispatchRequest::new(
        ticket("ENG-405", "Fail Workspace preparation"),
        RunMode::Foreground,
    );
    let dispatch = repository.direct_dispatch();
    let reservation = dispatch.reserve(&request).expect("reserve Worker");

    let error = dispatch
        .dispatch_reserved(reservation, &request)
        .expect_err("missing Workspace must fail before launch");

    assert!(matches!(error, DirectDispatchError::Workspace(_)));
    assert_eq!(
        repository
            .worker_pool()
            .snapshot()
            .worker(worker.as_str())
            .expect("released Worker")
            .status(),
        WorkerStatus::Idle
    );
}

#[test]
fn direct_dispatch_preserves_primary_and_release_conflict_without_clobbering_newer_state() {
    let (_temporary_directory, repository) = local_repository();
    let worker = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("grow Worker Pool")
        .added_workers()[0]
        .clone();
    let request = DirectDispatchRequest::new(
        ticket("ENG-405A", "Preserve compensation conflicts"),
        RunMode::Foreground,
    );
    let dispatch = repository.direct_dispatch();
    let reservation = dispatch.reserve(&request).expect("reserve Worker");
    let worker_state = repository.state_store().worker(worker.clone());
    let Loaded::Present(versioned) = worker_state.load().expect("Worker state") else {
        panic!("Worker state must exist");
    };
    let revision = versioned.revision().clone();
    let mut state = versioned.value;
    state.error = Some("newer mutation".to_owned());
    assert!(matches!(
        worker_state
            .commit(Expected::Match(revision), StateChange::Replace(state))
            .expect("write newer Worker state"),
        CommitOutcome::Applied(_)
    ));
    let workspace = repository
        .root()
        .parent()
        .expect("Repository parent")
        .join(format!(
            "{}-workspaces/{worker}",
            repository
                .root()
                .file_name()
                .expect("Repository name")
                .to_string_lossy()
        ));
    fs::remove_dir_all(workspace).expect("remove Worker Workspace");

    let error = dispatch
        .dispatch_reserved(reservation, &request)
        .expect_err("Workspace and compensation must fail");

    assert!(matches!(
        error,
        DirectDispatchError::ReservationRelease { primary, release }
            if matches!(*primary, DirectDispatchError::Workspace(_))
                && matches!(&release, WorkerPoolError::ReleaseConflict { worker: rejected } if rejected == &worker)
    ));
    let snapshot = repository.worker_pool().snapshot();
    let current = snapshot
        .worker(worker.as_str())
        .expect("newer Worker state");
    assert_eq!(current.status(), WorkerStatus::Busy);
    assert_eq!(current.error(), Some("newer mutation"));
}

#[test]
fn direct_dispatch_releases_its_reservation_after_identity_failure() {
    let (_temporary_directory, repository) = local_repository();
    configure_jj_identity(&repository);
    create_main(&repository);
    let worker = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("grow Worker Pool")
        .added_workers()[0]
        .clone();
    let request = DirectDispatchRequest::new(
        ticket("ENG-406", "Fail identity resolution"),
        RunMode::Foreground,
    );
    let dispatch = repository.direct_dispatch();
    let reservation = dispatch.reserve(&request).expect("reserve Worker");

    let error = dispatch
        .dispatch_reserved(reservation, &request)
        .expect_err("missing Repository remote must fail before launch");

    assert!(matches!(error, DirectDispatchError::Identity(_)));
    assert_eq!(
        repository
            .worker_pool()
            .snapshot()
            .worker(worker.as_str())
            .expect("released Worker")
            .status(),
        WorkerStatus::Idle
    );
}

#[test]
fn direct_dispatch_releases_its_reservation_after_prompt_failure() {
    let (_temporary_directory, repository) = local_repository();
    configure_jj_identity(&repository);
    add_origin(&repository);
    create_main(&repository);
    let worker = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("grow Worker Pool")
        .added_workers()[0]
        .clone();
    let pool = repository.state_store().pool();
    let Loaded::Present(versioned) = pool.load().expect("Pool state") else {
        panic!("Pool state must exist");
    };
    let revision = versioned.revision().clone();
    let mut state = versioned.value;
    state.agent = Some(WireAgent::new("codex"));
    assert!(matches!(
        pool.commit(Expected::Match(revision), StateChange::Replace(state))
            .expect("configure Codex Pool"),
        CommitOutcome::Applied(_)
    ));
    let request = DirectDispatchRequest::new(
        ticket("ENG-407", "Fail prompt construction"),
        RunMode::Foreground,
    )
    .with_budget(DispatchBudget::maximum_usd(1).expect("budget"));
    let dispatch = repository.direct_dispatch();
    let reservation = dispatch.reserve(&request).expect("reserve Worker");

    let error = dispatch
        .dispatch_reserved(reservation, &request)
        .expect_err("unsupported Codex budget must fail before launch");

    assert!(matches!(error, DirectDispatchError::Prompt(_)));
    assert_eq!(
        repository
            .worker_pool()
            .snapshot()
            .worker(worker.as_str())
            .expect("released Worker")
            .status(),
        WorkerStatus::Idle
    );
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

fn configure_jj_identity(repository: &Repository) {
    for arguments in [
        ["config", "set", "--repo", "user.email", "owner@example.com"],
        ["config", "set", "--repo", "user.name", "Owner Person"],
    ] {
        let output = Command::new("jj")
            .args(arguments)
            .current_dir(repository.root())
            .output()
            .expect("configure jj identity");
        assert!(output.status.success());
    }
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
    assert!(output.status.success());
}

fn create_main(repository: &Repository) {
    let output = Command::new("jj")
        .args(["bookmark", "create", "main", "-r", "@"])
        .current_dir(repository.root())
        .output()
        .expect("create main");
    assert!(output.status.success());
}

fn ticket(id: &str, title: &str) -> Ticket {
    Ticket::new(
        TicketId::parse(id).expect("Ticket ID"),
        TicketTitle::parse(title).expect("Ticket title"),
        TicketStatus::parse("Todo").expect("Ticket status"),
    )
}
