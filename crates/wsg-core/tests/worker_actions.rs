use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;
use wsg_core::{
    AgentSessionResolution, Expected, FollowUpExecution, Loaded, PoolCapacity, Repository, RunMode,
    StateChange, WireAgent, WireStatus, WireTimestamp, WorkerActions, WorkerId,
};

const HELPER_REPOSITORY: &str = "WSG_ACTION_REPOSITORY";
const HELPER_WORKER: &str = "WSG_ACTION_WORKER";
const HELPER_RESULT: &str = "WSG_ACTION_RESULT";

#[test]
fn send_rejects_a_busy_worker_through_the_actions_facade() {
    let (temporary_directory, repository) = local_repository();
    let growth = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("Worker Workspace should be provisioned");
    let worker = growth.added_workers()[0].clone();
    repository
        .worker_pool()
        .reserve_named(worker.clone(), "ENG-301")
        .expect("initial Run reservation");

    let result =
        WorkerActions::new(repository).send(&worker, "continue the work", RunMode::Background);

    assert!(result.is_err());
    drop(temporary_directory);
}

#[test]
fn review_rejects_a_worker_without_a_branch_before_launch() {
    let (_temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);

    let error = WorkerActions::new(repository)
        .review(&worker, RunMode::Background)
        .expect_err("review without a branch should fail");

    assert!(error.to_string().contains("has no branch"));
}

#[test]
fn review_builds_one_provider_neutral_follow_up_from_pr_state() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let prior_log = repository.root().join("review-prior.log");
    fs::write(
        &prior_log,
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"review-session\"}\n",
    )
    .expect("prior Session log");
    set_terminal_worker(&repository, &worker, &prior_log);
    let bin = temporary_directory.path().join("review-bin");
    fs::create_dir(&bin).expect("fake executable directory");
    let captured = temporary_directory.path().join("review-prompt");
    write_executable(
        &bin.join("gh"),
        "#!/bin/sh\ncase \"$*\" in\n  *\"pr list\"*) printf '%s\\n' '[{\"number\":42,\"url\":\"https://example/pr/42\",\"headRefName\":\"owner/eng-301-action\",\"mergeable\":\"CONFLICTING\",\"reviewDecision\":\"CHANGES_REQUESTED\"}]' ;;\n  *\"pr checks\"*) printf '%s\\n' '[{\"name\":\"tests\",\"conclusion\":\"FAILURE\"},{\"name\":\"lint\",\"conclusion\":\"SUCCESS\"}]'; exit 1 ;;\nesac\n",
    );
    write_executable(
        &bin.join("claude"),
        &format!(
            "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf '%s\\n' \"$@\" > {}\nprintf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}}'\n",
            captured.display()
        ),
    );
    let result = temporary_directory.path().join("review-result");
    let path = env::join_paths([
        bin.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("review PATH");
    let output = Command::new(env::current_exe().expect("test executable"))
        .args(["--exact", "review_action_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .output()
        .expect("Review helper should run");
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(result).expect("Review result"),
        "session=resumed:review-session; completed=true"
    );
    let prompt = fs::read_to_string(captured).expect("captured Review prompt");
    assert!(prompt.contains("Current review state: changes requested"));
    assert!(prompt.contains("has merge conflicts"));
    assert!(prompt.contains("tests"));
    assert!(!prompt.contains("   - lint"));
}

#[test]
fn send_resumes_the_prior_claude_session_and_reports_it() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let prior_log = repository
        .root()
        .join(".jj/pool")
        .join(format!("{worker}.log"));
    fs::write(
        &prior_log,
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"session-301\"}\n",
    )
    .expect("prior Session log");
    set_terminal_worker(&repository, &worker, &prior_log);

    let bin = temporary_directory.path().join("bin");
    fs::create_dir(&bin).expect("fake executable directory");
    let captured = temporary_directory.path().join("captured-args");
    write_executable(
        &bin.join("claude"),
        &format!(
            "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf '%s\\n' \"$@\" > {}\nprintf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}}'\n",
            captured.display()
        ),
    );
    let result = temporary_directory.path().join("result");
    let path = env::join_paths([
        bin.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let output = Command::new(env::current_exe().expect("test executable"))
        .args(["--exact", "send_action_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .output()
        .expect("Send helper should run");
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(result).expect("Send result"),
        "runtime=claude; session=resumed:session-301; completed=true"
    );
    let args = fs::read_to_string(captured).expect("captured Claude arguments");
    assert!(args.contains("--resume\nsession-301\n--fork-session"));
}

#[test]
fn send_on_an_idle_worker_starts_fresh_and_reports_the_reason() {
    let (temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let bin = temporary_directory.path().join("bin");
    fs::create_dir(&bin).expect("fake executable directory");
    let captured = temporary_directory.path().join("captured-fresh-args");
    write_executable(
        &bin.join("claude"),
        &format!(
            "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf '%s\\n' \"$@\" > {}\nprintf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}}'\n",
            captured.display()
        ),
    );
    let result = temporary_directory.path().join("fresh-result");
    let path = env::join_paths([
        bin.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let output = Command::new(env::current_exe().expect("test executable"))
        .args(["--exact", "send_action_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .output()
        .expect("fresh Send helper should run");
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(result).expect("fresh Send result"),
        "runtime=claude; session=fresh:no prior session log; completed=true"
    );
    let args = fs::read_to_string(captured).expect("captured fresh Claude arguments");
    assert!(args.contains("--append-system-prompt"));
    assert!(!args.contains("--resume"));
}

#[test]
fn failed_send_launch_restores_the_prior_terminal_worker() {
    let (_temporary_directory, repository) = local_repository();
    let worker = grow_one_worker(&repository);
    let prior_log = repository.root().join("prior.log");
    fs::write(
        &prior_log,
        "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"session-rollback\"}\n",
    )
    .expect("prior Session log");
    set_terminal_worker(&repository, &worker, &prior_log);

    let result = repository.root().join("failed-send-result");
    let path = env::join_paths([
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let output = Command::new(env::current_exe().expect("test executable"))
        .args(["--exact", "failed_send_action_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, repository.root())
        .env(HELPER_WORKER, worker.as_str())
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .output()
        .expect("failed Send helper should run");
    assert!(output.status.success());
    assert!(
        fs::read_to_string(result)
            .expect("failed Send result")
            .contains("claude")
    );

    let snapshot = repository.worker_pool().snapshot();
    let restored = snapshot.worker(worker.as_str()).expect("restored Worker");
    assert_eq!(restored.status(), wsg_core::WorkerStatus::Done);
    assert_eq!(restored.ticket(), Some("ENG-301"));
    assert_eq!(restored.branch_name(), Some("owner/eng-301-action"));
    assert_eq!(
        restored.log_file(),
        Some(prior_log.to_string_lossy().as_ref())
    );
}

#[test]
#[ignore]
fn review_action_helper() {
    let repository =
        Repository::open(env::var_os(HELPER_REPOSITORY).expect("repository")).expect("repository");
    let worker = WorkerId::parse(env::var(HELPER_WORKER).expect("Worker ID")).expect("Worker ID");
    let outcome = WorkerActions::new(repository)
        .review(&worker, RunMode::Foreground)
        .expect("Review should launch");
    let session = match outcome.session() {
        AgentSessionResolution::Resumed { session_id } => format!("resumed:{session_id}"),
        AgentSessionResolution::Fresh { reason } => format!("fresh:{reason}"),
    };
    let completed = matches!(outcome.execution(), FollowUpExecution::Foreground(_));
    fs::write(
        env::var_os(HELPER_RESULT).expect("result path"),
        format!("session={session}; completed={completed}"),
    )
    .expect("Review result");
}

#[test]
#[ignore]
fn failed_send_action_helper() {
    let repository =
        Repository::open(env::var_os(HELPER_REPOSITORY).expect("repository")).expect("repository");
    let worker = WorkerId::parse(env::var(HELPER_WORKER).expect("Worker ID")).expect("Worker ID");
    let error = WorkerActions::new(repository)
        .send(&worker, "continue", RunMode::Background)
        .expect_err("missing runtime should fail");
    fs::write(
        env::var_os(HELPER_RESULT).expect("result path"),
        error.to_string(),
    )
    .expect("failed Send result");
}

#[test]
#[ignore]
fn send_action_helper() {
    let repository =
        Repository::open(env::var_os(HELPER_REPOSITORY).expect("repository")).expect("repository");
    let worker = WorkerId::parse(env::var(HELPER_WORKER).expect("Worker ID")).expect("Worker ID");
    let outcome = WorkerActions::new(repository)
        .send(&worker, "continue the work", RunMode::Foreground)
        .expect("Send should launch");
    let session = match outcome.session() {
        AgentSessionResolution::Resumed { session_id } => format!("resumed:{session_id}"),
        AgentSessionResolution::Fresh { reason } => format!("fresh:{reason}"),
    };
    let completed = matches!(outcome.execution(), FollowUpExecution::Foreground(_));
    fs::write(
        env::var_os(HELPER_RESULT).expect("result path"),
        format!(
            "runtime={}; session={session}; completed={completed}",
            outcome.runtime().as_str()
        ),
    )
    .expect("Send result");
}

fn grow_one_worker(repository: &Repository) -> WorkerId {
    repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("Worker Workspace should be provisioned")
        .added_workers()[0]
        .clone()
}

fn set_terminal_worker(repository: &Repository, worker: &WorkerId, prior_log: &Path) {
    let state_repository = repository.state_store().worker(worker.clone());
    let loaded = match state_repository.load().expect("Worker state") {
        Loaded::Present(versioned) => versioned,
        Loaded::Missing => panic!("Worker state should exist"),
    };
    let (mut state, revision) = loaded.into_parts();
    state.status = WireStatus::new("done");
    state.agent = Some(WireAgent::new("claude"));
    state.ticket = Some("ENG-301".to_owned());
    state.started_at = Some(WireTimestamp::new("2026-07-31T10:00:00Z"));
    state.completed_at = Some(WireTimestamp::new("2026-07-31T10:05:00Z"));
    state.log_file = Some(prior_log.to_string_lossy().into_owned());
    state.branch_name = Some("owner/eng-301-action".to_owned());
    state.exit_code = Some(0);
    let outcome = state_repository
        .commit(Expected::Match(revision), StateChange::Replace(state))
        .expect("terminal Worker state");
    assert!(matches!(outcome, wsg_core::CommitOutcome::Applied(_)));
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("fake executable");
    let mut permissions = fs::metadata(path)
        .expect("fake executable metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("fake executable permissions");
}

fn local_repository() -> (TempDir, Repository) {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let output = Command::new("jj")
        .args(["--config", "signing.behavior=drop", "git", "init"])
        .arg(temporary_directory.path())
        .output()
        .expect("jj should be installed");
    assert!(
        output.status.success(),
        "jj init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new("jj")
        .args([
            "git",
            "remote",
            "add",
            "origin",
            "git@github.com:owner/repo.git",
        ])
        .current_dir(temporary_directory.path())
        .output()
        .expect("jj remote add should run");
    assert!(
        output.status.success(),
        "jj remote add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let repository = Repository::open(temporary_directory.path()).expect("repository");
    (temporary_directory, repository)
}
