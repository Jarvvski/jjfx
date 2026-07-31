use std::process::Command;

use tempfile::TempDir;
use wsg_core::{
    DirectDispatchExecution, DirectDispatchFailure, DirectDispatchFailurePhase,
    DirectDispatchOutcome, DirectDispatchRequest, DirectDispatchResult, DirectDispatchSuccess,
    DispatchBudget, DispatchDependencyContext, PoolCapacity, Repository, RunMode, Ticket, TicketId,
    TicketStatus, TicketTitle, WorkerId, WorkerStatus,
};

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
