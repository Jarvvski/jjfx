#![cfg(unix)]

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use jjfx::engine::{
    AgentModel, CommitOutcome, ConcurrencyCap, DirectDispatchError, DirectDispatchExecution,
    DirectDispatchFailure, DirectDispatchFailurePhase, DirectDispatchOutcome,
    DirectDispatchRequest, DirectDispatchResult, DirectDispatchSuccess, DispatchBudget,
    DispatchDependencyContext, Expected, Loaded, PoolCapacity, Repository, RunMode, RunSupervisor,
    StateChange, Ticket, TicketId, TicketStatus, TicketTitle, WireAgent, WorkerId, WorkerPoolError,
    WorkerStatus,
};
use jjfx::test_support::LIVENESS_TIMEOUT;
use serde_json::Value;
use tempfile::TempDir;

mod support;

const HELPER_REPOSITORY: &str = "JJFX_DIRECT_DISPATCH_REPOSITORY";
const HELPER_RESULT: &str = "JJFX_DIRECT_DISPATCH_RESULT";
const HELPER_CAPTURE: &str = "JJFX_DIRECT_DISPATCH_CAPTURE";
const HELPER_BUSY_STATE: &str = "JJFX_DIRECT_DISPATCH_BUSY_STATE";
const HELPER_RELEASE: &str = "JJFX_DIRECT_DISPATCH_RELEASE";
const HELPER_CAP: &str = "JJFX_DIRECT_DISPATCH_CAP";

#[test]
fn request_for_ticket_id_constructs_a_valid_direct_request() {
    let id = TicketId::parse("AMBA-42").expect("Ticket ID");
    let request = DirectDispatchRequest::for_ticket_id(id, RunMode::Foreground)
        .expect("Ticket ID creates a valid request");

    assert_eq!(request.ticket().id().as_str(), "AMBA-42");
    assert_eq!(request.ticket().title().as_str(), "AMBA-42");
    assert_eq!(request.ticket().status().as_str(), "Todo");
    assert_eq!(request.mode(), RunMode::Foreground);
}

/// A capped Dispatch with no room left fires nothing, so it never reaches the
/// Agent Runtime and needs no fake one on PATH.
#[test]
fn a_capped_dispatch_holds_a_batch_back_when_the_pool_is_full() {
    let (_temporary_directory, repository) = local_repository();
    configure_jj_identity(&repository);
    add_origin(&repository);
    create_main(&repository);
    repository
        .worker_pool()
        .resize_to(PoolCapacity::new(3).expect("capacity"))
        .expect("grow Worker Pool");
    // Two Workers are already running, so a cap of two leaves no room at all.
    assert_eq!(
        repository
            .worker_pool()
            .reserve_many(&["ENG-1", "ENG-2"])
            .expect("reserve the running Workers")
            .len(),
        2
    );

    let requests = ["ENG-10", "ENG-11", "ENG-12"].map(|id| {
        DirectDispatchRequest::for_ticket_id(
            TicketId::parse(id).expect("Ticket ID"),
            RunMode::Background,
        )
        .expect("a direct request")
    });
    let result = repository
        .direct_dispatch()
        .with_concurrency_cap(ConcurrencyCap::new(2))
        .dispatch(&requests)
        .expect("a fully held batch never reaches the Agent Runtime");

    // One outcome per requested Ticket, in input order, and none of them fired.
    assert_eq!(result.outcomes().len(), 3);
    assert_eq!(
        result
            .waiting()
            .iter()
            .map(|waiting| waiting.ticket().id().as_str())
            .collect::<Vec<_>>(),
        ["ENG-10", "ENG-11", "ENG-12"]
    );
    assert!(result.waiting()[0].detail().contains("cap 2"));

    // Nothing was reserved: the free Worker is still idle, and the two running
    // Workers still hold their own Tickets.
    let snapshot = repository.worker_pool().snapshot();
    assert_eq!(
        snapshot
            .waiting()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["ENG-10", "ENG-11", "ENG-12"]
    );
    assert_eq!(
        snapshot
            .workers()
            .iter()
            .filter(|worker| worker.status() == WorkerStatus::Idle)
            .count(),
        1
    );
    // The held-back Tickets are queued durably too: they are work jjfx accepted
    // and did not run, so they must survive this process rather than living only
    // in the waiting set the next Dispatch rewrites.
    let queue = repository.work_queue().items().expect("read the queue");
    assert_eq!(
        queue
            .iter()
            .map(|item| (item.step.as_str(), item.state))
            .collect::<Vec<_>>(),
        [
            ("dispatch:ENG-10", jjfx::engine::QueueItemState::Queued),
            ("dispatch:ENG-11", jjfx::engine::QueueItemState::Queued),
            ("dispatch:ENG-12", jjfx::engine::QueueItemState::Queued)
        ]
    );
    assert!(
        queue[0]
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("cap 2")),
        "a queued step must record why it did not run: {:?}",
        queue[0].detail
    );
}

#[test]
fn a_capped_dispatch_fires_the_prefix_that_fits_and_holds_the_rest() {
    let (temporary_directory, repository) = local_repository();
    configure_jj_identity(&repository);
    add_origin(&repository);
    create_main(&repository);
    repository
        .worker_pool()
        .resize_to(PoolCapacity::new(2).expect("capacity"))
        .expect("grow Worker Pool");
    // One Worker is already running, so a cap of two leaves room for exactly one
    // more Run: a two-Ticket batch fires its first and holds its second.
    assert_eq!(
        repository
            .worker_pool()
            .reserve_many(&["ENG-1"])
            .expect("reserve the running Worker")
            .len(),
        1
    );
    let bin = temporary_directory.path().join("bin");
    fs::create_dir(&bin).expect("runtime bin directory");
    write_executable(
        &bin.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'\n",
    );
    let mut paths = vec![bin];
    paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
    let path = env::join_paths(paths).expect("runtime PATH");
    let result = temporary_directory.path().join("capped-result");
    let output = Command::new(env::current_exe().expect("test executable"))
        .args(["--exact", "capped_direct_dispatch_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAP, "2")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run capped Direct Dispatch helper");
    assert!(
        output.status.success(),
        "capped Direct Dispatch helper failed with {}:\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(result).expect("capped Dispatch result"),
        "fired=1;waiting=ENG-11"
    );
    // The waiting set outlives the process that wrote it: it is how a later
    // `status` or TUI shows the backlog.
    assert_eq!(
        repository
            .worker_pool()
            .snapshot()
            .waiting()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["ENG-11"]
    );
}

#[test]
#[ignore]
fn capped_direct_dispatch_helper() {
    let repository = Repository::open(env::var_os(HELPER_REPOSITORY).expect("Repository path"))
        .expect("open Repository");
    let cap = ConcurrencyCap::new(
        env::var(HELPER_CAP)
            .expect("cap")
            .parse()
            .expect("cap number"),
    )
    .expect("a usable cap");
    let requests = ["ENG-10", "ENG-11"].map(|id| {
        DirectDispatchRequest::new(ticket(id, "Capped batch ticket"), RunMode::Foreground)
    });
    let result = repository
        .direct_dispatch()
        .with_concurrency_cap(Some(cap))
        .dispatch(&requests)
        .expect("capped Direct Dispatch");
    let fired = result
        .outcomes()
        .iter()
        .filter(|outcome| !matches!(outcome, DirectDispatchOutcome::Waiting(_)))
        .count();
    let waiting = result
        .waiting()
        .iter()
        .map(|waiting| waiting.ticket().id().as_str().to_owned())
        .collect::<Vec<_>>()
        .join(",");
    fs::write(
        env::var_os(HELPER_RESULT).expect("Dispatch result"),
        format!("fired={fired};waiting={waiting}"),
    )
    .expect("write capped Dispatch result");
}

#[test]
fn unsupported_budget_fails_before_worker_reservation() {
    let (_temporary_directory, repository) = local_repository();
    configure_jj_identity(&repository);
    add_origin(&repository);
    create_main(&repository);
    repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("grow Worker Pool");
    configure_pool_runtime(&repository, "codex");
    let request = DirectDispatchRequest::new(
        ticket("ENG-499", "Reject unsupported budget"),
        RunMode::Foreground,
    )
    .with_budget(DispatchBudget::MaximumUsd(1));

    let result = repository.direct_dispatch().reserve(&request);

    let error = match result {
        Err(error) => error,
        Ok(reservation) => {
            RunSupervisor::new()
                .reset_run(&repository, reservation.worker_id())
                .expect("reset unexpected Reservation");
            panic!("unsupported budget reserved a Worker")
        }
    };
    assert!(error.to_string().contains("Dispatch spending override"));
    assert_eq!(
        repository.worker_pool().snapshot().workers()[0].status(),
        WorkerStatus::Idle
    );
}

#[test]
fn failed_runtime_launch_releases_the_direct_dispatch_reservation() {
    let (temporary_directory, repository) = local_repository();
    configure_jj_identity(&repository);
    add_origin(&repository);
    create_main(&repository);
    repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("grow Worker Pool");
    let bin = temporary_directory.path().join("isolated-bin");
    fs::create_dir(&bin).expect("isolated runtime bin");
    write_executable(
        &bin.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then /bin/rm \"$0\"; exit 0; fi\nexit 1\n",
    );
    let jj = env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("jj"))
        .find(|path| path.is_file())
        .expect("jj executable on PATH");
    std::os::unix::fs::symlink(jj, bin.join("jj")).expect("isolated jj executable");
    let result = temporary_directory.path().join("failed-launch-result");
    let output = Command::new(env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "failed_runtime_launch_direct_dispatch_helper",
            "--ignored",
        ])
        .env("PATH", &bin)
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run failed-launch Direct Dispatch helper");
    assert!(
        output.status.success(),
        "failed-launch helper failed with {}:\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(result).expect("failed-launch result"),
        "idle"
    );
}

#[test]
#[ignore]
fn failed_runtime_launch_direct_dispatch_helper() {
    let repository = Repository::open(env::var_os(HELPER_REPOSITORY).expect("Repository path"))
        .expect("open Repository");
    let request = DirectDispatchRequest::new(
        ticket("ENG-502", "Compensate failed launch"),
        RunMode::Foreground,
    );
    let dispatch = repository.direct_dispatch();
    let reservation = dispatch.reserve(&request).expect("Reservation");
    let worker = reservation.worker_id().clone();
    let error = dispatch
        .dispatch_reserved(reservation, &request)
        .expect_err("removed Runtime executable must fail launch");
    assert!(
        matches!(error, DirectDispatchError::Runtime(_)),
        "unexpected failed-launch error: {error:?}"
    );
    let status = repository
        .worker_pool()
        .snapshot()
        .worker(worker.as_str())
        .expect("released Worker")
        .status();
    fs::write(
        env::var_os(HELPER_RESULT).expect("failed-launch result"),
        status.as_str(),
    )
    .expect("write failed-launch result");
}

#[test]
fn production_direct_dispatch_launches_foreground_and_background_fake_runtimes() {
    let (temporary_directory, repository) = local_repository();
    configure_jj_identity(&repository);
    add_origin(&repository);
    create_main(&repository);
    repository
        .worker_pool()
        .resize_to(PoolCapacity::new(2).expect("capacity"))
        .expect("grow Worker Pool");
    let bin = temporary_directory.path().join("bin");
    fs::create_dir(&bin).expect("runtime bin directory");
    let release = temporary_directory.path().join("release-background");
    let capture = temporary_directory.path().join("captured-arguments");
    let busy_state = temporary_directory.path().join("busy-state.json");
    let result = temporary_directory.path().join("dispatch-result");
    write_executable(
        &bin.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf '%s\\n' \"$@\" >> \"$JJFX_DIRECT_DISPATCH_CAPTURE\"\ncase \"$*\" in *ENG-501*) while [ ! -f \"$JJFX_DIRECT_DISPATCH_RELEASE\" ]; do sleep 0.02; done ;; esac\nprintf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'\n",
    );
    let mut paths = vec![bin];
    paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
    let path = env::join_paths(paths).expect("runtime PATH");
    let output = Command::new(env::current_exe().expect("test executable"))
        .args(["--exact", "production_direct_dispatch_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_RESULT, &result)
        .env(HELPER_CAPTURE, &capture)
        .env(HELPER_BUSY_STATE, &busy_state)
        .env(HELPER_RELEASE, &release)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run production Direct Dispatch helper");
    assert!(
        output.status.success(),
        "Direct Dispatch helper failed with {}:\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let result = fs::read_to_string(result).expect("Dispatch result");
    assert!(result.contains("foreground=Some(0)"));
    assert!(result.contains("background_pid="));
    let captured = fs::read_to_string(capture).expect("captured Runtime arguments");
    assert!(captured.contains("owner/repo"));
    assert!(captured.contains("owner@example.com"));
    assert!(captured.contains("STACKED BRANCH"));
    assert!(captured.contains("--model"));
    assert!(captured.contains("opus"));
    let worker_state: Value =
        serde_json::from_slice(&fs::read(&busy_state).expect("busy Worker state"))
            .expect("legacy Worker JSON");
    assert_eq!(worker_state["status"], "busy");
    assert_eq!(worker_state["agent"], "claude");
    assert_eq!(worker_state["ticket"], "ENG-501");
    assert!(worker_state["pid"].as_u64().is_some_and(|pid| pid > 0));
    assert!(worker_state["started_at"].is_string());
    assert!(worker_state["log_file"].is_string());
}

#[test]
#[ignore]
fn production_direct_dispatch_helper() {
    let repository = Repository::open(env::var_os(HELPER_REPOSITORY).expect("Repository path"))
        .expect("open Repository");
    let dispatch = repository.direct_dispatch();
    let dependency = DispatchDependencyContext::new(
        vec!["main".to_owned()],
        "- main provides the prerequisite implementation",
        "main",
    );
    let foreground_request = DirectDispatchRequest::new(
        ticket("ENG-500", "Launch foreground Runtime"),
        RunMode::Foreground,
    )
    .with_model("opus")
    .with_dependency_context(dependency.clone());
    let foreground = dispatch
        .dispatch_reserved(
            dispatch
                .reserve(&foreground_request)
                .expect("foreground Reservation"),
            &foreground_request,
        )
        .expect("foreground Direct Dispatch");
    let DirectDispatchExecution::Foreground(completed) = foreground.execution() else {
        panic!("expected foreground completion");
    };

    let background_request = DirectDispatchRequest::new(
        ticket("ENG-501", "Launch background Runtime"),
        RunMode::Background,
    )
    .with_dependency_context(dependency);
    let background = dispatch
        .dispatch_reserved(
            dispatch
                .reserve(&background_request)
                .expect("background Reservation"),
            &background_request,
        )
        .expect("background Direct Dispatch");
    let DirectDispatchExecution::Background { pid } = background.execution() else {
        panic!("expected background PID");
    };
    let state_path = repository
        .root()
        .join(".jj/pools/default")
        .join(format!("{}.json", background.worker()));
    fs::copy(
        state_path,
        env::var_os(HELPER_BUSY_STATE).expect("busy state result"),
    )
    .expect("capture busy Worker state");
    fs::write(env::var_os(HELPER_RELEASE).expect("background release"), [])
        .expect("release background Runtime");
    let deadline = Instant::now() + LIVENESS_TIMEOUT;
    loop {
        let snapshot = repository.worker_pool().snapshot();
        let status = snapshot
            .worker(background.worker().as_str())
            .expect("background Worker")
            .status();
        if status == WorkerStatus::Done {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "background waiter did not finalize"
        );
        thread::sleep(Duration::from_millis(10));
    }
    fs::write(
        env::var_os(HELPER_RESULT).expect("Dispatch result"),
        format!(
            "foreground={:?};background_pid={pid}",
            completed.exit_code()
        ),
    )
    .expect("write Dispatch result");
}

#[test]
fn concurrent_direct_dispatch_claims_never_double_assign_one_worker() {
    let (_temporary_directory, repository) = local_repository();
    repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("grow Worker Pool");
    let barrier = Arc::new(Barrier::new(2));
    let claimed = thread::scope(|scope| {
        let handles = ["ENG-503", "ENG-504"].map(|id| {
            let dispatch = repository.direct_dispatch();
            let barrier = Arc::clone(&barrier);
            scope.spawn(move || {
                let request = DirectDispatchRequest::new(
                    ticket(id, "Concurrent Direct Dispatch claim"),
                    RunMode::Background,
                );
                barrier.wait();
                dispatch.reserve(&request).is_ok()
            })
        });
        handles
            .into_iter()
            .map(|handle| handle.join().expect("claim thread"))
            .filter(|claimed| *claimed)
            .count()
    });

    assert_eq!(claimed, 1);
    assert_eq!(
        repository
            .worker_pool()
            .snapshot()
            .workers()
            .iter()
            .filter(|worker| worker.status() == WorkerStatus::Busy)
            .count(),
        1
    );
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
fn a_batch_naming_one_ticket_twice_is_rejected_before_reserving_any_worker() {
    let (_temporary_directory, repository) = local_repository();
    configure_jj_identity(&repository);
    add_origin(&repository);
    create_main(&repository);
    repository
        .worker_pool()
        .resize_to(PoolCapacity::new(2).expect("capacity"))
        .expect("grow Worker Pool");
    let dispatch = repository.direct_dispatch();
    let request =
        DirectDispatchRequest::new(ticket("ENG-501", "Dispatch me once"), RunMode::Foreground);

    let error = dispatch
        .dispatch(&[request.clone(), request])
        .expect_err("a duplicate Ticket must not reserve two Workers");

    assert!(
        error.to_string().contains("duplicate Ticket ENG-501"),
        "unexpected error: {error}"
    );
    let snapshot = repository.worker_pool().snapshot();
    assert!(
        snapshot
            .workers()
            .iter()
            .all(|worker| worker.status() == WorkerStatus::Idle),
        "a rejected batch must reserve nothing"
    );
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
    // The failure names the missing remote and the command that adds it, rather
    // than a format complaint about an empty slug.
    let message = error.to_string();
    assert!(message.contains("origin"), "{message}");
    assert!(message.contains("jj git remote add origin"), "{message}");
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
    let ticket = ticket("ENG-407", "Fail prompt construction");
    let reservable = DirectDispatchRequest::new(ticket.clone(), RunMode::Foreground);
    let request = DirectDispatchRequest::new(ticket, RunMode::Foreground)
        .with_budget(DispatchBudget::maximum_usd(1).expect("budget"));
    let dispatch = repository.direct_dispatch();
    let reservation = dispatch.reserve(&reservable).expect("reserve Worker");
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
    configure_jj_identity(&repository);
    create_main(&repository);
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
    let request = DirectDispatchRequest::new(
        ticket("ENG-412", "Prepare Worker on main"),
        RunMode::Foreground,
    );
    let dispatch = repository.direct_dispatch();
    let reservation = dispatch.reserve(&request).expect("Reservation");

    let error = dispatch
        .dispatch_reserved(reservation, &request)
        .expect_err("missing remote must fail after Workspace preparation");

    assert!(matches!(error, DirectDispatchError::Identity(_)));
    assert!(
        repository
            .root()
            .join(".jj/pools/default")
            .join(format!("{worker}.workspace.lock"))
            .is_file()
    );
    let parent = Command::new("jj")
        .args(["log", "-r", "@-", "--no-graph", "-T", "bookmarks"])
        .current_dir(workspace)
        .output()
        .expect("read prepared parent");
    assert!(parent.status.success());
    assert!(String::from_utf8_lossy(&parent.stdout).contains("main"));
}

/// A fresh repository has no bookmarks at all, so a hard-coded `main` base fails
/// Workspace preparation. Dispatch must instead base the Worker on the default
/// Workspace's working copy, then fail (if at all) on the genuinely missing
/// piece - here, the remote - naming its fix.
#[test]
fn direct_dispatch_prepares_a_worker_workspace_without_a_main_bookmark() {
    let (_temporary_directory, repository) = local_repository();
    configure_jj_identity(&repository);
    // Deliberately neither `create_main` nor `add_origin`: the repository is as
    // fresh as `jj git init` leaves it.
    let worker = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("grow Worker Pool")
        .added_workers()[0]
        .clone();
    let workspace = worker_workspace(&repository, &worker);
    let request = DirectDispatchRequest::new(
        ticket("ENG-420", "Dispatch with no main bookmark"),
        RunMode::Foreground,
    );
    let dispatch = repository.direct_dispatch();
    let reservation = dispatch.reserve(&request).expect("Reservation");

    let error = dispatch
        .dispatch_reserved(reservation, &request)
        .expect_err("a repository with no origin must fail at identity");

    assert!(
        matches!(error, DirectDispatchError::Identity(_)),
        "an absent `main` bookmark must not fail Workspace preparation: {error}"
    );
    assert!(
        error.to_string().contains("jj git remote add origin"),
        "the failure must name the remote fix: {error}"
    );

    // The Worker Workspace was prepared on the default Workspace's working-copy
    // commit, so provisioning got past the absent `main` bookmark.
    assert_eq!(
        working_copy_parent(&workspace),
        working_copy_commit(repository.root())
    );
}

#[test]
fn direct_dispatch_growth_requires_and_honors_exact_caller_approval() {
    let (_temporary_directory, repository) = local_repository();
    configure_jj_identity(&repository);
    create_main(&repository);
    repository
        .worker_pool()
        .resize_to(PoolCapacity::new(0).expect("empty capacity"))
        .expect("create empty Worker Pool");
    let requests = [DirectDispatchRequest::new(
        ticket("ENG-413", "Grow approved capacity"),
        RunMode::Foreground,
    )];

    let shortage = repository
        .direct_dispatch()
        .dispatch_with_approved_growth(&requests, 0)
        .expect_err("growth beyond approval must be rejected");
    assert!(matches!(
        shortage,
        DirectDispatchError::WorkerPool(WorkerPoolError::CapacityShortage(shortage))
            if shortage.gap() == 1
    ));
    let result = repository
        .direct_dispatch()
        .dispatch_with_approved_growth(&requests, 1)
        .expect("approved growth and Reservation");

    assert!(matches!(
        &result.outcomes()[0],
        DirectDispatchOutcome::Failed(failure)
            if failure.phase() == DirectDispatchFailurePhase::Identity
    ));
    let snapshot = repository.worker_pool().snapshot();
    assert_eq!(snapshot.pool().expect("Pool").size(), 1);
    assert_eq!(snapshot.workers()[0].status(), WorkerStatus::Idle);
}

#[test]
fn reserved_handoff_validates_the_ticket_before_workspace_mutation() {
    let (_temporary_directory, repository) = local_repository();
    let worker = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("grow Worker Pool")
        .added_workers()[0]
        .clone();
    let reserved_request =
        DirectDispatchRequest::new(ticket("ENG-414", "Reserved Ticket"), RunMode::Foreground);
    let mismatched_request =
        DirectDispatchRequest::new(ticket("ENG-415", "Different Ticket"), RunMode::Foreground)
            .with_dependency_context(DispatchDependencyContext::new(
                vec!["missing-revision".to_owned()],
                "mismatched dependency",
                "main",
            ));
    let dispatch = repository.direct_dispatch();
    let reservation = dispatch
        .reserve(&reserved_request)
        .expect("Reservation for first Ticket");

    let error = dispatch
        .dispatch_reserved(reservation, &mismatched_request)
        .expect_err("mismatched handoff must fail before Workspace preparation");

    assert!(matches!(
        error,
        DirectDispatchError::ReservationTicketMismatch { reserved, requested }
            if reserved == "ENG-414" && requested == "ENG-415"
    ));
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
fn default_bulk_dispatch_is_all_or_nothing_before_any_workspace_or_prompt_work() {
    let (_temporary_directory, repository) = local_repository();
    let worker = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("grow Worker Pool")
        .added_workers()[0]
        .clone();
    let requests = [
        DirectDispatchRequest::new(ticket("ENG-408", "First bulk Ticket"), RunMode::Foreground),
        DirectDispatchRequest::new(ticket("ENG-409", "Second bulk Ticket"), RunMode::Foreground),
    ];

    let error = repository
        .direct_dispatch()
        .dispatch(&requests)
        .expect_err("default bulk Dispatch must reject a shortage atomically");

    assert!(matches!(
        error,
        DirectDispatchError::WorkerPool(WorkerPoolError::CapacityShortage(shortage))
            if shortage.requested() == 2 && shortage.available() == 1
    ));
    assert_eq!(
        repository
            .worker_pool()
            .snapshot()
            .worker(worker.as_str())
            .expect("unclaimed Worker")
            .status(),
        WorkerStatus::Idle
    );
}

#[test]
fn explicit_use_available_dispatch_preserves_ticket_order_and_capacity_failures() {
    let (_temporary_directory, repository) = local_repository();
    configure_jj_identity(&repository);
    create_main(&repository);
    repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("grow Worker Pool");
    let requests = [
        DirectDispatchRequest::new(
            ticket("ENG-410", "Claim available Worker"),
            RunMode::Foreground,
        ),
        DirectDispatchRequest::new(
            ticket("ENG-411", "Report unavailable Worker"),
            RunMode::Foreground,
        ),
    ];

    let result = repository
        .direct_dispatch()
        .dispatch_use_available(&requests)
        .expect("explicit partial Dispatch");

    assert!(result.is_partial());
    assert_eq!(result.outcomes()[0].ticket().id().as_str(), "ENG-410");
    assert_eq!(result.outcomes()[1].ticket().id().as_str(), "ENG-411");
    assert!(matches!(
        &result.outcomes()[0],
        DirectDispatchOutcome::Failed(failure)
            if failure.phase() == DirectDispatchFailurePhase::Identity
                && failure.worker().is_some()
    ));
    assert!(matches!(
        &result.outcomes()[1],
        DirectDispatchOutcome::Failed(failure)
            if failure.phase() == DirectDispatchFailurePhase::Capacity
                && failure.worker().is_none()
    ));
    assert_eq!(
        repository
            .worker_pool()
            .snapshot()
            .workers()
            .iter()
            .next()
            .expect("Worker")
            .status(),
        WorkerStatus::Idle
    );
}

#[test]
fn direct_dispatch_request_preserves_provider_aware_model_selection() {
    let request = DirectDispatchRequest::new(
        ticket("ENG-400", "Preserve provider-aware model selection"),
        RunMode::Background,
    )
    .with_model(AgentModel::new("gpt-5.4").with_provider("openai"));

    let model = request.model().expect("provider-aware model selection");
    assert_eq!(model.provider(), Some("openai"));
    assert_eq!(model.model(), "gpt-5.4");
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
    assert_eq!(request.model().map(AgentModel::model), Some("opus"));
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
        .args([
            "--config",
            "signing.behavior=drop",
            "git",
            "init",
            "--no-colocate",
        ])
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
        ["config", "set", "--repo", "signing.behavior", "drop"],
    ] {
        let output = Command::new("jj")
            .args(arguments)
            .current_dir(repository.root())
            .output()
            .expect("configure jj identity");
        assert!(output.status.success());
    }
}

fn configure_pool_runtime(repository: &Repository, runtime: &str) {
    let pool = repository.state_store().pool();
    let Loaded::Present(versioned) = pool.load().expect("Pool state") else {
        panic!("Pool state must exist");
    };
    let revision = versioned.revision().clone();
    let mut state = versioned.value;
    state.agent = Some(WireAgent::new(runtime));
    assert!(matches!(
        pool.commit(Expected::Match(revision), StateChange::Replace(state))
            .expect("configure Pool runtime"),
        CommitOutcome::Applied(_)
    ));
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

/// The Worker Workspace path the base-directory convention derives for `worker`.
fn worker_workspace(repository: &Repository, worker: &WorkerId) -> PathBuf {
    repository
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
        ))
}

/// The commit `@` resolves to in `dir`, read without snapshotting it.
fn working_copy_commit(dir: &Path) -> String {
    jj_stdout(dir, &["log", "-r", "@", "--no-graph", "-T", "commit_id"])
}

/// The parent commit of `dir`'s working copy, read without snapshotting it.
fn working_copy_parent(dir: &Path) -> String {
    jj_stdout(dir, &["log", "-r", "@-", "--no-graph", "-T", "commit_id"])
}

fn jj_stdout(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("jj")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("jj");
    assert!(
        output.status.success(),
        "jj {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write executable");
    let mut permissions = fs::metadata(path)
        .expect("executable metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("executable permissions");
}

fn ticket(id: &str, title: &str) -> Ticket {
    Ticket::new(
        TicketId::parse(id).expect("Ticket ID"),
        TicketTitle::parse(title).expect("Ticket title"),
        TicketStatus::parse("Todo").expect("Ticket status"),
    )
}
