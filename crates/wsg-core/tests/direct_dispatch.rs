#![cfg(unix)]

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;
use wsg_core::{
    AgentModel, CommitOutcome, DirectDispatchError, DirectDispatchExecution, DirectDispatchFailure,
    DirectDispatchFailurePhase, DirectDispatchOutcome, DirectDispatchRequest, DirectDispatchResult,
    DirectDispatchSuccess, DispatchBudget, DispatchDependencyContext, Expected, Loaded,
    PoolCapacity, Repository, RunMode, RunSupervisor, StateChange, Ticket, TicketId, TicketStatus,
    TicketTitle, WireAgent, WorkerId, WorkerPoolError, WorkerStatus,
};

const HELPER_REPOSITORY: &str = "WSG_DIRECT_DISPATCH_REPOSITORY";
const HELPER_RESULT: &str = "WSG_DIRECT_DISPATCH_RESULT";
const HELPER_CAPTURE: &str = "WSG_DIRECT_DISPATCH_CAPTURE";
const HELPER_COMPATIBILITY: &str = "WSG_DIRECT_DISPATCH_COMPATIBILITY";
const HELPER_RELEASE: &str = "WSG_DIRECT_DISPATCH_RELEASE";
const HELPER_AGENT_DIR: &str = "WSG_DIRECT_DISPATCH_AGENT_DIR";
const HELPER_RUNTIME_MARKER: &str = "WSG_DIRECT_DISPATCH_RUNTIME_MARKER";
const HELPER_PROFILE_FIXTURE: &str = "WSG_DIRECT_DISPATCH_PROFILE_FIXTURE";
const HELPER_PROFILE_BEHAVIOR: &str = "WSG_DIRECT_DISPATCH_PROFILE_BEHAVIOR";
const HELPER_PROFILE_DESCENDANT: &str = "WSG_DIRECT_DISPATCH_PROFILE_DESCENDANT";
const HELPER_MODEL_PROVIDER: &str = "WSG_DIRECT_DISPATCH_MODEL_PROVIDER";
const HELPER_OPERATION: &str = "WSG_DIRECT_DISPATCH_OPERATION";

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

#[test]
fn missing_pi_dispatch_profile_fails_before_worker_reservation() {
    let (temporary_directory, repository) = local_repository();
    configure_jj_identity(&repository);
    add_origin(&repository);
    create_main(&repository);
    repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("grow Worker Pool");
    configure_pool_runtime(&repository, "pi");
    let bin = temporary_directory.path().join("isolated-bin");
    fs::create_dir(&bin).expect("isolated runtime bin");
    let marker = temporary_directory.path().join("runtime-started");
    write_executable(
        &bin.join("pi"),
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 0.84.1; exit 0; fi\nif [ \"$1\" = \"--help\" ]; then echo '--mode --provider --model --session --session-dir --system-prompt --name --tools --no-extensions --no-skills --no-prompt-templates --no-themes --no-context-files --no-approve'; exit 0; fi\ntouch \"$WSG_DIRECT_DISPATCH_RUNTIME_MARKER\"\nexit 0\n",
    );
    let jj = env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("jj"))
        .find(|path| path.is_file())
        .expect("jj executable on PATH");
    std::os::unix::fs::symlink(jj, bin.join("jj")).expect("isolated jj executable");
    let result = temporary_directory.path().join("missing-profile-result");
    let agent_dir = temporary_directory.path().join("empty-pi-agent");
    fs::create_dir(&agent_dir).expect("empty Pi agent directory");

    let output = Command::new(env::current_exe().expect("test executable"))
        .args(["--exact", "missing_pi_dispatch_profile_helper", "--ignored"])
        .env("PATH", &bin)
        .env("PI_CODING_AGENT_DIR", &agent_dir)
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_RESULT, &result)
        .env(HELPER_AGENT_DIR, &agent_dir)
        .env(HELPER_RUNTIME_MARKER, &marker)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run missing-profile Direct Dispatch helper");

    assert!(
        output.status.success(),
        "missing-profile helper failed with {}:\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result = fs::read_to_string(result).expect("missing-profile result");
    assert!(result.starts_with("idle|"), "unexpected result: {result}");
    assert!(result.contains("pi-mcp-adapter 2.11.0"));
    assert!(
        !marker.exists(),
        "Pi runtime started before profile preflight"
    );
}

#[test]
#[ignore]
fn missing_pi_dispatch_profile_helper() {
    let repository = Repository::open(env::var_os(HELPER_REPOSITORY).expect("Repository path"))
        .expect("open Repository");
    let provider = env::var(HELPER_MODEL_PROVIDER).unwrap_or_else(|_| "openai".to_owned());
    let mut model = AgentModel::new("gpt-5.4");
    if !provider.is_empty() {
        model = model.with_provider(provider);
    }
    let request = DirectDispatchRequest::new(
        ticket("ENG-498", "Require Pi Linear profile"),
        RunMode::Foreground,
    )
    .with_model(model);
    let dispatch = repository.direct_dispatch();
    let operation = env::var(HELPER_OPERATION).unwrap_or_else(|_| "dispatch".to_owned());
    let detail = if operation == "reserve" {
        match dispatch.reserve(&request) {
            Err(error) => error.to_string(),
            Ok(reservation) => {
                RunSupervisor::new()
                    .reset_run(&repository, reservation.worker_id())
                    .expect("reset unexpected Reservation");
                "unexpected reservation".to_owned()
            }
        }
    } else if operation == "growth" {
        let second = DirectDispatchRequest::new(
            ticket("ENG-499", "Require Pi Linear profile twice"),
            RunMode::Foreground,
        )
        .with_model(AgentModel::new("gpt-5.4").with_provider("openai"));
        match dispatch.dispatch_with_approved_growth(&[request, second], 1) {
            Err(error) => error.to_string(),
            Ok(_) => "unexpected success".to_owned(),
        }
    } else {
        match dispatch.dispatch(&[request]) {
            Err(error) => error.to_string(),
            Ok(result) => match &result.outcomes()[0] {
                DirectDispatchOutcome::Failed(failure) => failure.detail().to_owned(),
                DirectDispatchOutcome::Succeeded(_) if operation == "success" => {
                    "success".to_owned()
                }
                DirectDispatchOutcome::Succeeded(_) => "unexpected success".to_owned(),
            },
        }
    };
    let status = repository
        .worker_pool()
        .snapshot()
        .workers()
        .first()
        .expect("Worker")
        .status();
    fs::write(
        env::var_os(HELPER_RESULT).expect("missing-profile result"),
        format!(
            "{}|{detail}|{}",
            status.as_str(),
            repository.worker_pool().snapshot().workers().len()
        ),
    )
    .expect("write missing-profile result");
}

#[test]
fn missing_pi_profile_fails_before_approved_pool_growth() {
    let (result, runtime_started, _) =
        run_pi_profile_preflight_request_case(None, None, "fixture", "openai", "growth");

    assert!(result.starts_with("idle|"), "unexpected result: {result}");
    assert!(result.contains("pi-mcp-adapter 2.11.0"));
    assert!(
        result.ends_with("|1"),
        "Pool grew before preflight: {result}"
    );
    assert!(
        !runtime_started,
        "Pi runtime started before profile preflight"
    );
}

#[test]
fn unsupported_pi_mcp_adapter_version_fails_before_worker_reservation() {
    let (result, runtime_started, _) = run_pi_profile_preflight_case(
        Some(r#"{"name":"pi-mcp-adapter","version":"2.10.0"}"#),
        None,
        "fixture",
    );

    assert!(result.starts_with("idle|"), "unexpected result: {result}");
    assert!(result.contains("requires pi-mcp-adapter 2.11.0"));
    assert!(result.contains("found pi-mcp-adapter 2.10.0"));
    assert!(
        !runtime_started,
        "Pi runtime started before profile preflight"
    );
}

#[test]
fn missing_pi_linear_tool_fails_before_worker_reservation() {
    let fixture = r#"{
        "allTools": [
            {"name":"linear_get_issue","parameters":{"type":"object","properties":{"id":{"type":"string"}}}},
            {"name":"linear_update_issue","parameters":{"type":"object","properties":{"id":{"type":"string"},"status":{"type":"string"},"assignee":{"type":"string"}}}}
        ],
        "activeTools": ["linear_get_issue","linear_update_issue"]
    }"#;
    let (result, runtime_started, _) = run_pi_profile_preflight_case(
        Some(r#"{"name":"pi-mcp-adapter","version":"2.11.0"}"#),
        Some(fixture),
        "fixture",
    );

    assert!(result.starts_with("idle|"), "unexpected result: {result}");
    assert!(
        result.contains("linear_create_comment"),
        "unexpected result: {result}"
    );
    assert!(
        !runtime_started,
        "Pi runtime started before profile preflight"
    );
}

#[test]
fn pi_profile_probe_timeout_reaps_descendants_before_worker_reservation() {
    let (result, runtime_started, descendant) = run_pi_profile_preflight_case(
        Some(r#"{"name":"pi-mcp-adapter","version":"2.11.0"}"#),
        None,
        "hang",
    );

    assert!(result.starts_with("idle|"), "unexpected result: {result}");
    assert!(
        result.contains("timed out after 10 seconds"),
        "unexpected result: {result}"
    );
    assert!(
        !runtime_started,
        "Pi runtime started before profile preflight"
    );
    let descendant = descendant.expect("profile probe descendant PID");
    assert!(
        !Command::new("/bin/kill")
            .args(["-0", &descendant.to_string()])
            .status()
            .expect("probe descendant liveness")
            .success(),
        "profile probe descendant {descendant} survived timeout"
    );
}

#[test]
fn valid_pi_profile_reaches_the_run_supervisor() {
    let fixture = r#"{
        "allTools": [
            {"name":"linear_get_issue","parameters":{"type":"object","properties":{"id":{"type":"string"}}}},
            {"name":"linear_update_issue","parameters":{"type":"object","properties":{"id":{"type":"string"},"status":{"type":"string"},"assignee":{"type":"string"}}}},
            {"name":"linear_create_comment","parameters":{"type":"object","properties":{"issueId":{"type":"string"},"body":{"type":"string"}}}}
        ],
        "activeTools": ["linear_get_issue","linear_update_issue","linear_create_comment"]
    }"#;
    let (result, runtime_started, _) = run_pi_profile_preflight_request_case(
        Some(r#"{"name":"pi-mcp-adapter","version":"2.11.0"}"#),
        Some(fixture),
        "fixture",
        "openai",
        "success",
    );

    assert!(
        result.starts_with("done|success|"),
        "unexpected result: {result}"
    );
    assert!(
        runtime_started,
        "Pi runtime did not start after valid preflight"
    );
}

#[test]
fn incompatible_pi_linear_schema_fails_before_worker_reservation() {
    let fixture = r#"{
        "allTools": [
            {"name":"linear_get_issue","parameters":{"type":"object","properties":{"id":{"type":"string"}}}},
            {"name":"linear_update_issue","parameters":{"type":"object","properties":{"id":{"type":"string"},"status":{"type":"string"}}}},
            {"name":"linear_create_comment","parameters":{"type":"object","properties":{"issueId":{"type":"string"},"body":{"type":"string"}}}}
        ],
        "activeTools": ["linear_get_issue","linear_update_issue","linear_create_comment"]
    }"#;
    let (result, runtime_started, _) = run_pi_profile_preflight_case(
        Some(r#"{"name":"pi-mcp-adapter","version":"2.11.0"}"#),
        Some(fixture),
        "fixture",
    );

    assert!(result.starts_with("idle|"), "unexpected result: {result}");
    assert!(result.contains("linear_update_issue"));
    assert!(result.contains("assignee"));
    assert!(
        !runtime_started,
        "Pi runtime started before profile preflight"
    );
}

#[test]
fn missing_pi_model_provider_fails_before_worker_reservation() {
    let fixture = r#"{
        "allTools": [
            {"name":"linear_get_issue","parameters":{"type":"object","properties":{"id":{"type":"string"}}}},
            {"name":"linear_update_issue","parameters":{"type":"object","properties":{"id":{"type":"string"},"status":{"type":"string"},"assignee":{"type":"string"}}}},
            {"name":"linear_create_comment","parameters":{"type":"object","properties":{"issueId":{"type":"string"},"body":{"type":"string"}}}}
        ],
        "activeTools": ["linear_get_issue","linear_update_issue","linear_create_comment"]
    }"#;
    let (result, runtime_started, _) = run_pi_profile_preflight_request_case(
        Some(r#"{"name":"pi-mcp-adapter","version":"2.11.0"}"#),
        Some(fixture),
        "fixture",
        "",
        "reserve",
    );

    assert!(result.starts_with("idle|"), "unexpected result: {result}");
    assert!(
        result.contains("requires a model provider"),
        "unexpected result: {result}"
    );
    assert!(
        !runtime_started,
        "Pi runtime started before profile preflight"
    );
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
    let compatibility = temporary_directory
        .path()
        .join("compatible-busy-state.json");
    let result = temporary_directory.path().join("dispatch-result");
    write_executable(
        &bin.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf '%s\\n' \"$@\" >> \"$WSG_DIRECT_DISPATCH_CAPTURE\"\ncase \"$*\" in *ENG-501*) while [ ! -f \"$WSG_DIRECT_DISPATCH_RELEASE\" ]; do sleep 0.02; done ;; esac\nprintf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'\n",
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
        .env(HELPER_COMPATIBILITY, &compatibility)
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
    let compatible: Value =
        serde_json::from_slice(&fs::read(&compatibility).expect("compatible busy Worker state"))
            .expect("Go-compatible Worker JSON");
    assert_eq!(compatible["status"], "busy");
    assert_eq!(compatible["agent"], "claude");
    assert_eq!(compatible["ticket"], "ENG-501");
    assert!(compatible["pid"].as_u64().is_some_and(|pid| pid > 0));
    assert!(compatible["started_at"].is_string());
    assert!(compatible["log_file"].is_string());

    if let Some(go_helper) = env::var_os("WSG_GO_TEST_BINARY") {
        let go_result = temporary_directory.path().join("go-observed-worker.json");
        let observed = Command::new(&go_helper)
            .arg("-test.run")
            .arg("^TestStateLockSubprocessHelper$")
            .env("WSG_STATE_HELPER_MODE", "rewrite")
            .env("WSG_STATE_HELPER_KIND", "worker")
            .env("WSG_STATE_HELPER_STATE", &compatibility)
            .env("WSG_STATE_HELPER_RESULT", &go_result)
            .output()
            .expect("run Go wsg typed Worker reader");
        assert!(
            observed.status.success(),
            "Go wsg could not observe Rust-launched state: {}",
            String::from_utf8_lossy(&observed.stderr)
        );
        let observed: Value =
            serde_json::from_slice(&fs::read(go_result).expect("Go-observed Worker state"))
                .expect("Go-observed Worker JSON");
        assert_eq!(observed["status"], "busy");
        assert_eq!(observed["ticket"], "ENG-501");

        let reconciled = Command::new(go_helper)
            .arg("-test.run")
            .arg("^TestLoadLiveWorkerReconcilesDeadBusyWorker$")
            .output()
            .expect("run Go wsg reconciliation contract");
        assert!(
            reconciled.status.success(),
            "Go wsg reconciliation contract failed: {}",
            String::from_utf8_lossy(&reconciled.stderr)
        );
    }
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
        .join(".jj/pool")
        .join(format!("{}.json", background.worker()));
    fs::copy(
        state_path,
        env::var_os(HELPER_COMPATIBILITY).expect("compatibility result"),
    )
    .expect("capture Go-compatible busy state");
    fs::write(env::var_os(HELPER_RELEASE).expect("background release"), [])
        .expect("release background Runtime");
    let deadline = Instant::now() + Duration::from_secs(5);
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
            .join(".jj/pool")
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
        ticket("ENG-400", "Preserve Pi model profile"),
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

fn run_pi_profile_preflight_case(
    manifest: Option<&str>,
    profile_fixture: Option<&str>,
    profile_behavior: &str,
) -> (String, bool, Option<u32>) {
    run_pi_profile_preflight_request_case(
        manifest,
        profile_fixture,
        profile_behavior,
        "openai",
        "dispatch",
    )
}

fn run_pi_profile_preflight_request_case(
    manifest: Option<&str>,
    profile_fixture: Option<&str>,
    profile_behavior: &str,
    model_provider: &str,
    operation: &str,
) -> (String, bool, Option<u32>) {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let repository_path = temporary_directory.path().join("repository");
    fs::create_dir(&repository_path).expect("repository directory");
    let output = Command::new("jj")
        .args(["--config", "signing.behavior=drop", "git", "init"])
        .arg(&repository_path)
        .output()
        .expect("jj init");
    assert!(output.status.success(), "jj init failed: {output:?}");
    let repository = Repository::open(&repository_path).expect("open repository");
    configure_jj_identity(&repository);
    add_origin(&repository);
    create_main(&repository);
    repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("grow Worker Pool");
    configure_pool_runtime(&repository, "pi");

    let bin = temporary_directory.path().join("isolated-bin");
    fs::create_dir(&bin).expect("isolated runtime bin");
    let marker = temporary_directory.path().join("runtime-started");
    write_executable(
        &bin.join("pi"),
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo 0.84.1; exit 0; fi
if [ "$1" = "--help" ]; then echo '--mode --provider --model --session --session-dir --system-prompt --name --tools --no-extensions --no-skills --no-prompt-templates --no-themes --no-context-files --no-approve'; exit 0; fi
if [ "$1" = "--mode" ] && [ "$2" = "rpc" ]; then
  if [ "$WSG_DIRECT_DISPATCH_PROFILE_BEHAVIOR" = "hang" ]; then
    (trap '' TERM; while :; do /bin/sleep 0.05; done) &
    printf '%s\n' "$!" > "$WSG_DIRECT_DISPATCH_PROFILE_DESCENDANT"
    wait
  fi
  /bin/cp "$WSG_DIRECT_DISPATCH_PROFILE_FIXTURE" "$JJFX_PI_PROFILE_PROBE_OUTPUT"
  exit 0
fi
/usr/bin/touch "$WSG_DIRECT_DISPATCH_RUNTIME_MARKER"
printf '%s\n' '{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"done"}],"provider":"openai","model":"gpt-5.4","stopReason":"stop"}}'
exit 0
"#,
    );
    let jj = env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("jj"))
        .find(|path| path.is_file())
        .expect("jj executable on PATH");
    std::os::unix::fs::symlink(jj, bin.join("jj")).expect("isolated jj executable");
    let result = temporary_directory.path().join("profile-result");
    let agent_dir = temporary_directory.path().join("pi-agent");
    fs::create_dir(&agent_dir).expect("Pi agent directory");
    if let Some(manifest) = manifest {
        let package = agent_dir.join("npm/node_modules/pi-mcp-adapter");
        fs::create_dir_all(&package).expect("Pi MCP adapter package directory");
        fs::write(package.join("package.json"), manifest).expect("Pi MCP adapter manifest");
        fs::write(package.join("index.ts"), "export default function () {}\n")
            .expect("Pi MCP adapter entry");
    }
    let fixture = temporary_directory.path().join("profile-fixture.json");
    if let Some(profile_fixture) = profile_fixture {
        fs::write(&fixture, profile_fixture).expect("Pi profile fixture");
    }
    let descendant = temporary_directory.path().join("profile-descendant");

    let output = Command::new(env::current_exe().expect("test executable"))
        .args(["--exact", "missing_pi_dispatch_profile_helper", "--ignored"])
        .env("PATH", &bin)
        .env("PI_CODING_AGENT_DIR", &agent_dir)
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_RESULT, &result)
        .env(HELPER_RUNTIME_MARKER, &marker)
        .env(HELPER_PROFILE_FIXTURE, &fixture)
        .env(HELPER_PROFILE_BEHAVIOR, profile_behavior)
        .env(HELPER_PROFILE_DESCENDANT, &descendant)
        .env(HELPER_MODEL_PROVIDER, model_provider)
        .env(HELPER_OPERATION, operation)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run Pi profile Direct Dispatch helper");
    assert!(
        output.status.success(),
        "Pi profile helper failed with {}:\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (
        fs::read_to_string(result).expect("Pi profile result"),
        marker.exists(),
        fs::read_to_string(descendant)
            .ok()
            .and_then(|value| value.trim().parse().ok()),
    )
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
