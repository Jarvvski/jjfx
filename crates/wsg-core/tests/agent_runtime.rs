use std::env;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{
    Pid, Signal, kill_process_group, test_kill_process, test_kill_process_group,
};
use serde_json::Value;
use tempfile::TempDir;
use wsg_core::{
    AgentRuntime, AgentRuntimeCapabilities, AgentRuntimeInvocation, AgentRuntimeProbeError,
    Expected, PoolCapacity, Repository, RunRequest, RunReset, RunSupervisor, RunSupervisorError,
    StateChange, WireStatus, WorkerId, WorkerStatus,
};

const HELPER_MODE: &str = "WSG_AGENT_RUNTIME_HELPER_MODE";
const HELPER_PATH: &str = "WSG_AGENT_RUNTIME_HELPER_PATH";
const HELPER_RESULT: &str = "WSG_AGENT_RUNTIME_HELPER_RESULT";
const HELPER_WORKSPACE: &str = "WSG_AGENT_RUNTIME_HELPER_WORKSPACE";
const HELPER_LOG: &str = "WSG_AGENT_RUNTIME_HELPER_LOG";
const HELPER_EXECUTED: &str = "WSG_AGENT_RUNTIME_HELPER_EXECUTED";
const HELPER_RUNTIME: &str = "WSG_AGENT_RUNTIME_HELPER_RUNTIME";
const HELPER_PROCESS: &str = "WSG_AGENT_RUNTIME_HELPER_PROCESS";
const HELPER_RELEASE: &str = "WSG_AGENT_RUNTIME_HELPER_RELEASE";
const HELPER_EXIT: &str = "WSG_AGENT_RUNTIME_HELPER_EXIT";
const HELPER_REPOSITORY: &str = "WSG_AGENT_RUNTIME_HELPER_REPOSITORY";
const HELPER_WORKER: &str = "WSG_AGENT_RUNTIME_HELPER_WORKER";
const HELPER_RESERVED: &str = "WSG_AGENT_RUNTIME_HELPER_RESERVED";
const HELPER_PROCEED: &str = "WSG_AGENT_RUNTIME_HELPER_PROCEED";
const HELPER_GRACEFUL: &str = "WSG_AGENT_RUNTIME_HELPER_GRACEFUL";
const HELPER_DIAGNOSTIC: &str = "WSG_AGENT_RUNTIME_HELPER_DIAGNOSTIC";
const HELPER_TERM: &str = "WSG_AGENT_RUNTIME_HELPER_TERM";
const HELPER_LAUNCH: &str = "WSG_AGENT_RUNTIME_HELPER_LAUNCH";
const HELPER_DESCENDANT: &str = "WSG_AGENT_RUNTIME_HELPER_DESCENDANT";

#[test]
fn fresh_claude_command_preserves_headless_stream_invocation() {
    let invocation = AgentRuntimeInvocation::new("implement the thing")
        .with_model("opus")
        .with_name("pool:worker-abc:AMBA-42")
        .with_system_prompt("dispatch rules");
    let command =
        AgentRuntime::Claude.command(&invocation, AgentRuntimeCapabilities::new(false, true));

    assert_eq!(command.get_program(), "claude");
    let args = command_args(&command);
    assert_eq!(
        &args[..12],
        [
            "-p",
            "--model",
            "opus",
            "--output-format",
            "stream-json",
            "--verbose",
            "--forward-subagent-text",
            "--settings",
            r#"{"permissions":{"defaultMode":"auto"}}"#,
            "--name",
            "pool:worker-abc:AMBA-42",
            "--append-system-prompt",
        ]
    );
    assert!(args[12].starts_with("dispatch rules\n\nDelegated work is read-only."));
    assert_eq!(args[13], "implement the thing");
}

#[test]
fn resumed_claude_command_does_not_repeat_system_prompt() {
    let invocation = AgentRuntimeInvocation::new("fix the tests")
        .with_model("opus")
        .with_session_id("sess-abc-123")
        .with_system_prompt("must not be repeated");
    let command = AgentRuntime::Claude.command(&invocation, AgentRuntimeCapabilities::default());

    let args = command_args(&command);
    assert_eq!(
        &args[..11],
        [
            "-p",
            "--model",
            "opus",
            "--resume",
            "sess-abc-123",
            "--fork-session",
            "--output-format",
            "stream-json",
            "--verbose",
            "--settings",
            r#"{"permissions":{"defaultMode":"auto"}}"#,
        ]
    );
    assert!(args[11].starts_with("Delegated work is read-only."));
    assert!(args[11].ends_with("\n\nfix the tests"));
    assert!(!args.iter().any(|arg| arg == "must not be repeated"));
}

#[test]
fn fresh_codex_command_preserves_workspace_dispatch_invocation() {
    let invocation = AgentRuntimeInvocation::new("implement it")
        .with_model("gpt-test")
        .with_system_prompt("system rules");
    let command =
        AgentRuntime::Codex.command(&invocation, AgentRuntimeCapabilities::new(true, false));

    assert_eq!(command.get_program(), "codex");
    let args = command_args(&command);
    assert_eq!(
        &args[..11],
        [
            "--sandbox",
            "workspace-write",
            "--ask-for-approval",
            "never",
            "--model",
            "gpt-test",
            "--enable",
            "multi_agent",
            "exec",
            "--json",
            "--skip-git-repo-check",
        ]
    );
    assert!(args[11].starts_with("system rules\n\nDelegated work is read-only."));
    assert!(args[11].ends_with("\n\nimplement it"));
}

#[test]
fn resumed_codex_command_does_not_repeat_system_prompt() {
    let invocation = AgentRuntimeInvocation::new("continue")
        .with_model("gpt-test")
        .with_session_id("thread-123")
        .with_system_prompt("must not be repeated");
    let command =
        AgentRuntime::Codex.command(&invocation, AgentRuntimeCapabilities::new(true, false));

    let args = command_args(&command);
    assert_eq!(
        &args[..13],
        [
            "--sandbox",
            "workspace-write",
            "--ask-for-approval",
            "never",
            "--model",
            "gpt-test",
            "--enable",
            "multi_agent",
            "exec",
            "resume",
            "--json",
            "--skip-git-repo-check",
            "thread-123",
        ]
    );
    assert!(args[13].starts_with("Delegated work is read-only."));
    assert!(args[13].ends_with("\n\ncontinue"));
    assert!(!args.iter().any(|arg| arg == "must not be repeated"));
}

#[test]
fn command_omits_capability_flags_when_not_supported_by_that_runtime() {
    let claude = AgentRuntime::Claude.command(
        &AgentRuntimeInvocation::new("work"),
        AgentRuntimeCapabilities::new(true, false),
    );
    let codex = AgentRuntime::Codex.command(
        &AgentRuntimeInvocation::new("work"),
        AgentRuntimeCapabilities::new(false, true),
    );

    assert!(
        !command_args(&claude)
            .iter()
            .any(|arg| arg == "--forward-subagent-text")
    );
    assert!(!command_args(&claude).iter().any(|arg| arg == "multi_agent"));
    assert!(
        !command_args(&codex)
            .iter()
            .any(|arg| arg == "--forward-subagent-text")
    );
    assert!(!command_args(&codex).iter().any(|arg| arg == "multi_agent"));
}

#[test]
fn missing_runtime_executable_is_reported_through_probe_interface() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let result = temporary_directory.path().join("result");
    let output = helper_command(temporary_directory.path(), &result, "missing")
        .output()
        .expect("probe helper should run");

    assert!(
        output.status.success(),
        "probe helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let error = fs::read_to_string(result).expect("probe helper result");
    assert_eq!(error, "claude executable not found in PATH");
}

#[test]
fn claude_probe_detects_forwarded_subagent_text() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    write_executable(
        &temporary_directory.path().join("claude"),
        "#!/bin/sh\nprintf '%s\\n' 'Usage: claude --forward-subagent-text'\n",
    );
    let result = temporary_directory.path().join("result");
    let output = helper_command(temporary_directory.path(), &result, "claude")
        .output()
        .expect("probe helper should run");

    assert!(
        output.status.success(),
        "probe helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(result).expect("probe helper result"),
        "forward=true multi=false"
    );
}

#[test]
fn codex_probe_detects_multi_agent_feature() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    write_executable(
        &temporary_directory.path().join("codex"),
        "#!/bin/sh\nprintf '%s\\n' 'multi_agent stable false'\n",
    );
    let result = temporary_directory.path().join("result");
    let output = helper_command(temporary_directory.path(), &result, "codex")
        .output()
        .expect("probe helper should run");

    assert!(
        output.status.success(),
        "probe helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(result).expect("probe helper result"),
        "forward=false multi=true"
    );
}

#[test]
fn failed_optional_probe_does_not_block_runtime_availability() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    write_executable(
        &temporary_directory.path().join("claude"),
        "#!/bin/sh\nprintf '%s\\n' '--forward-subagent-text'\nexit 1\n",
    );
    let result = temporary_directory.path().join("result");
    let output = helper_command(temporary_directory.path(), &result, "failed")
        .output()
        .expect("probe helper should run");

    assert!(
        output.status.success(),
        "probe helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(result).expect("probe helper result"),
        "forward=false multi=false"
    );
}

#[test]
fn probe_runs_in_the_worker_workspace() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let workspace_result = temporary_directory.path().join("workspace-result");
    write_executable(
        &temporary_directory.path().join("claude"),
        "#!/bin/sh\npwd > \"$WSG_AGENT_RUNTIME_HELPER_WORKSPACE\"\n",
    );
    let result = temporary_directory.path().join("result");
    let output = helper_command(temporary_directory.path(), &result, "workspace")
        .output()
        .expect("probe helper should run");

    assert!(
        output.status.success(),
        "probe helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(workspace_result)
            .expect("runtime should receive a workspace current directory")
            .trim(),
        temporary_directory
            .path()
            .canonicalize()
            .expect("canonical temporary path")
            .to_str()
            .expect("temporary path")
    );
}

#[test]
fn reserved_background_run_persists_pid_before_returning() {
    let temporary_directory = TempDir::new().expect("temporary directory");
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
    let growth = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("Worker Workspace should be provisioned");
    let worker_id = growth.added_workers()[0].clone();

    let bin_directory = temporary_directory.path().join("bin");
    let log = temporary_directory.path().join("worker.log");
    let result = temporary_directory.path().join("result");
    let process = temporary_directory.path().join("process");
    let release = temporary_directory.path().join("release");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    write_executable(
        &bin_directory.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf '%s %s\\n' \"$$\" \"$(ps -o pgid= -p $$ | tr -d ' ')\" > \"$WSG_AGENT_RUNTIME_HELPER_PROCESS\"\nwhile [ ! -f \"$WSG_AGENT_RUNTIME_HELPER_RELEASE\" ]; do sleep 0.02; done\n",
    );
    let path = env::join_paths([
        bin_directory.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args(["--exact", "reserved_background_run_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, temporary_directory.path())
        .env(HELPER_WORKER, worker_id.as_str())
        .env(HELPER_LOG, &log)
        .env(HELPER_RESULT, &result)
        .env(HELPER_PROCESS, &process)
        .env(HELPER_RELEASE, &release)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = BackgroundHelperGuard::spawn(&mut helper, &release, [&result, &process]);

    wait_for_file(&result);
    wait_for_file(&process);
    let pid: u32 = fs::read_to_string(&result)
        .expect("background PID")
        .parse()
        .expect("numeric background PID");
    let snapshot = repository.worker_pool().snapshot();
    let worker = snapshot
        .worker(worker_id.as_str())
        .expect("reserved Worker");
    assert_eq!(worker.status(), WorkerStatus::Busy);
    assert_eq!(worker.pid(), Some(pid));

    fs::write(&release, []).expect("release runtime");
    let output = child.wait_with_output();
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn reserved_background_run_releases_worker_when_runtime_probe_fails() {
    let temporary_directory = TempDir::new().expect("temporary directory");
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
    let growth = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("Worker Workspace should be provisioned");
    let worker_id = growth.added_workers()[0].clone();
    let bin_directory = temporary_directory.path().join("bin");
    let log = temporary_directory.path().join("worker.log");
    let result = temporary_directory.path().join("result");
    let process = temporary_directory.path().join("process");
    let release = temporary_directory.path().join("release");
    let reserved = temporary_directory.path().join("reserved");
    let proceed = temporary_directory.path().join("proceed");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    write_executable(
        &bin_directory.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then rm \"$0\"; exit 0; fi\n",
    );
    let path = env::join_paths([bin_directory.as_os_str()]).expect("runtime PATH");
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args(["--exact", "reserved_background_run_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, temporary_directory.path())
        .env(HELPER_WORKER, worker_id.as_str())
        .env(HELPER_LOG, &log)
        .env(HELPER_RESULT, &result)
        .env(HELPER_PROCESS, &process)
        .env(HELPER_RELEASE, &release)
        .env(HELPER_RESERVED, &reserved)
        .env(HELPER_PROCEED, &proceed)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = BackgroundHelperGuard::spawn(&mut helper, &release, [&result, &process]);
    wait_for_file(&reserved);
    fs::remove_file(bin_directory.join("claude")).expect("remove runtime before spawn");
    fs::write(&proceed, []).expect("allow launch to continue");
    wait_for_file(&result);
    let output = child.wait_with_output();
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let snapshot = repository.worker_pool().snapshot();
    let worker = snapshot
        .worker(worker_id.as_str())
        .expect("Worker snapshot");
    assert_eq!(
        worker.status(),
        WorkerStatus::Idle,
        "launch failure should release the Worker: {}",
        fs::read_to_string(&result).expect("launch failure")
    );
    assert_eq!(worker.ticket(), None);
    assert_eq!(worker.pid(), None);
}

#[test]
fn reserved_background_run_cleans_up_when_worker_state_disappears_before_pid_persistence() {
    let temporary_directory = TempDir::new().expect("temporary directory");
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
    let growth = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("Worker Workspace should be provisioned");
    let worker_id = growth.added_workers()[0].clone();

    let bin_directory = temporary_directory.path().join("bin");
    let log = temporary_directory.path().join("worker.log");
    let result = temporary_directory.path().join("result");
    let process = temporary_directory.path().join("process");
    let release = temporary_directory.path().join("release");
    let reserved = temporary_directory.path().join("reserved");
    let proceed = temporary_directory.path().join("proceed");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    write_executable(
        &bin_directory.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf '%s %s\\n' \"$$\" \"$(ps -o pgid= -p $$ | tr -d ' ')\" > \"$WSG_AGENT_RUNTIME_HELPER_PROCESS\"\nwhile [ ! -f \"$WSG_AGENT_RUNTIME_HELPER_RELEASE\" ]; do sleep 0.02; done\n",
    );
    let path = env::join_paths([
        bin_directory.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args(["--exact", "reserved_background_run_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, temporary_directory.path())
        .env(HELPER_WORKER, worker_id.as_str())
        .env(HELPER_LOG, &log)
        .env(HELPER_RESULT, &result)
        .env(HELPER_PROCESS, &process)
        .env(HELPER_RELEASE, &release)
        .env(HELPER_RESERVED, &reserved)
        .env(HELPER_PROCEED, &proceed)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = BackgroundHelperGuard::spawn(&mut helper, &release, [&result, &process]);

    wait_for_file(&reserved);
    let worker_state = repository.state_store().worker(worker_id.clone());
    let loaded = match worker_state.load().expect("reserved Worker state") {
        wsg_core::Loaded::Present(versioned) => versioned,
        wsg_core::Loaded::Missing => panic!("reserved Worker state should exist"),
    };
    let outcome = worker_state
        .commit(
            Expected::Match(loaded.revision().clone()),
            StateChange::Remove,
        )
        .expect("remove Worker state for the race");
    assert!(matches!(outcome, wsg_core::CommitOutcome::Applied(_)));
    fs::create_dir(
        temporary_directory
            .path()
            .join(".jj/pool")
            .join(format!("{worker_id}.json")),
    )
    .expect("replace Worker state with a directory");
    fs::write(&proceed, []).expect("allow launch to continue");

    wait_for_file(&process);
    wait_for_file(&result);
    let error = fs::read_to_string(&result).expect("persistence error");
    assert!(error.contains("Is a directory"), "{error}");
    let pid: i32 = fs::read_to_string(&process)
        .expect("runtime process identity")
        .split_whitespace()
        .next()
        .expect("runtime PID")
        .parse()
        .expect("numeric runtime PID");
    let pid = Pid::from_raw(pid).expect("runtime PID");
    let output = child.wait_with_output();
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let process_alive = test_kill_process(pid).is_ok();
    if process_alive {
        let _ = kill_process_group(pid, Signal::KILL);
    }
    assert!(
        !process_alive,
        "untracked runtime process should be cleaned up"
    );
}

#[test]
fn reserved_background_run_allows_graceful_group_shutdown_before_forcing() {
    let temporary_directory = TempDir::new().expect("temporary directory");
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
    let growth = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("Worker Workspace should be provisioned");
    let worker_id = growth.added_workers()[0].clone();

    let bin_directory = temporary_directory.path().join("bin");
    let result = temporary_directory.path().join("result");
    let process = temporary_directory.path().join("process");
    let reserved = temporary_directory.path().join("reserved");
    let proceed = temporary_directory.path().join("proceed");
    let graceful = temporary_directory.path().join("graceful");
    let diagnostic = temporary_directory.path().join("diagnostic");
    let term = temporary_directory.path().join("term");
    let launch = temporary_directory.path().join("launch");
    let descendant = temporary_directory.path().join("descendant");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    write_executable(
        &bin_directory.join("claude"),
        "#!/bin/bash\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\n( trap '' TERM; while :; do :; done ) &\nprintf '%s\\n' \"$!\" > \"$WSG_AGENT_RUNTIME_HELPER_DESCENDANT\"\ntrap 'printf term > \"$WSG_AGENT_RUNTIME_HELPER_TERM\"; deadline=$(( ${EPOCHREALTIME/./} + 600000 )); while (( ${EPOCHREALTIME/./} < deadline )); do :; done; printf graceful > \"$WSG_AGENT_RUNTIME_HELPER_GRACEFUL\"; exit 0' TERM\nprintf '%s %s\\n' \"$$\" \"$(ps -o pgid= -p $$ | tr -d ' ')\" > \"$WSG_AGENT_RUNTIME_HELPER_PROCESS\"\nprintf started > \"$WSG_AGENT_RUNTIME_HELPER_DIAGNOSTIC\"\nwhile [ ! -f \"$WSG_AGENT_RUNTIME_HELPER_LAUNCH\" ]; do :; done\nwhile :; do :; done\n",
    );
    let path = env::join_paths([
        bin_directory.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args(["--exact", "reserved_background_run_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, temporary_directory.path())
        .env(HELPER_WORKER, worker_id.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_PROCESS, &process)
        .env(HELPER_RESERVED, &reserved)
        .env(HELPER_PROCEED, &proceed)
        .env(HELPER_GRACEFUL, &graceful)
        .env(HELPER_DIAGNOSTIC, &diagnostic)
        .env(HELPER_TERM, &term)
        .env(HELPER_LAUNCH, &launch)
        .env(HELPER_DESCENDANT, &descendant)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let helper = BackgroundHelperGuard::spawn(&mut helper, &proceed, [&process, &result]);

    wait_for_file(&reserved);
    let worker_state = repository.state_store().worker(worker_id.clone());
    let loaded = match worker_state.load().expect("reserved Worker state") {
        wsg_core::Loaded::Present(versioned) => versioned,
        wsg_core::Loaded::Missing => panic!("reserved Worker state should exist"),
    };
    let outcome = worker_state
        .commit(
            Expected::Match(loaded.revision().clone()),
            StateChange::Remove,
        )
        .expect("remove Worker state for the race");
    assert!(matches!(outcome, wsg_core::CommitOutcome::Applied(_)));
    let worker_state_path = temporary_directory
        .path()
        .join(".jj/pool")
        .join(format!("{worker_id}.json"));
    let fifo_status = Command::new("mkfifo")
        .arg(&worker_state_path)
        .status()
        .expect("mkfifo should be installed");
    assert!(fifo_status.success(), "mkfifo failed: {fifo_status}");
    fs::write(&proceed, []).expect("allow launch to continue");
    wait_for_file(&diagnostic);
    wait_for_file(&descendant);
    fs::write(&worker_state_path, b"not-json").expect("release blocked Worker state read");
    fs::remove_file(&worker_state_path).expect("remove Worker state FIFO");
    fs::write(&worker_state_path, b"not-json").expect("preserve failed Worker state for release");
    fs::write(&launch, []).expect("allow runtime launch to continue");

    wait_for_file(&process);
    wait_for_file(&result);
    let error = fs::read_to_string(&result).expect("persistence error");
    assert!(error.contains("cannot persist PID"), "{error}");
    wait_for_file(&term);
    wait_for_file(&graceful);
    let pid: i32 = fs::read_to_string(&process)
        .expect("runtime process identity")
        .split_whitespace()
        .next()
        .expect("runtime PID")
        .parse()
        .expect("numeric runtime PID");
    let pid = Pid::from_raw(pid).expect("runtime PID");
    let deadline = Instant::now() + Duration::from_secs(3);
    while test_kill_process_group(pid).is_ok() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        test_kill_process_group(pid).is_err(),
        "runtime process group should be gone after graceful cleanup"
    );
    let descendant_pid = fs::read_to_string(&descendant)
        .expect("descendant process identity")
        .trim()
        .parse::<i32>()
        .expect("numeric descendant PID");
    let descendant_pid = Pid::from_raw(descendant_pid).expect("descendant PID");
    let deadline = Instant::now() + Duration::from_secs(3);
    while test_kill_process(descendant_pid).is_ok() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        test_kill_process(descendant_pid).is_err(),
        "stubborn descendant should be gone after group cleanup"
    );

    let output = helper.wait_with_output();
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn background_run_returns_process_group_leader_with_child_owned_log() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let bin_directory = temporary_directory.path().join("bin");
    let workspace = temporary_directory.path().join("workspace");
    let log = temporary_directory.path().join("worker.log");
    let result = temporary_directory.path().join("result");
    let process = temporary_directory.path().join("process");
    let release = temporary_directory.path().join("release");
    let exit = temporary_directory.path().join("exit");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    fs::create_dir(&workspace).expect("Worker Workspace");
    fs::write(&log, "stale output\n").expect("stale log");
    write_executable(
        &bin_directory.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf '%s %s\\n' \"$$\" \"$(ps -o pgid= -p $$ | tr -d ' ')\" > \"$WSG_AGENT_RUNTIME_HELPER_PROCESS\"\nprintf 'stdout-before\\n'\nprintf 'stderr-before\\n' >&2\nwhile [ ! -f \"$WSG_AGENT_RUNTIME_HELPER_RELEASE\" ]; do sleep 0.02; done\nprintf 'stdout-after\\n'\nprintf 'stderr-after\\n' >&2\n",
    );
    let path = env::join_paths([
        bin_directory.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args(["--exact", "background_run_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_WORKSPACE, &workspace)
        .env(HELPER_LOG, &log)
        .env(HELPER_RESULT, &result)
        .env(HELPER_PROCESS, &process)
        .env(HELPER_RELEASE, &release)
        .env(HELPER_EXIT, &exit)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = BackgroundHelperGuard::spawn(&mut helper, &release, [&result, &process]);

    wait_for_file(&result);
    wait_for_file(&process);
    assert!(
        child.is_running(),
        "background launch should return before the runtime exits"
    );
    let pid: u32 = fs::read_to_string(&result)
        .expect("background result")
        .parse()
        .expect("background PID");
    let process_identity = fs::read_to_string(&process).expect("process identity");
    let mut identity_parts = process_identity.split_whitespace();
    let runtime_pid: u32 = identity_parts
        .next()
        .expect("runtime PID")
        .parse()
        .expect("numeric runtime PID");
    let process_group: u32 = identity_parts
        .next()
        .expect("process group")
        .parse()
        .expect("numeric process group");
    assert_eq!(pid, runtime_pid);
    assert_eq!(pid, process_group);

    fs::write(&release, []).expect("release runtime");
    let output = child.wait_with_output();
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(exit).expect("background exit outcome"),
        "exit=Some(0)"
    );
    let log_contents = fs::read_to_string(log).expect("background log");
    assert!(!log_contents.contains("stale output"));
    assert!(log_contents.contains("stdout-before\n"));
    assert!(log_contents.contains("stderr-before\n"));
    assert!(log_contents.contains("stdout-after\n"));
    assert!(log_contents.contains("stderr-after\n"));
}

#[test]
fn background_run_log_setup_failure_prevents_workload_spawn() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let bin_directory = temporary_directory.path().join("bin");
    let workspace = temporary_directory.path().join("workspace");
    let missing_log = temporary_directory
        .path()
        .join("missing")
        .join("worker.log");
    let executed = temporary_directory.path().join("executed");
    let result = temporary_directory.path().join("result");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    fs::create_dir(&workspace).expect("Worker Workspace");
    write_executable(
        &bin_directory.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\ntouch \"$WSG_AGENT_RUNTIME_HELPER_EXECUTED\"\n",
    );
    let path = env::join_paths([
        bin_directory.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args(["--exact", "background_run_error_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_WORKSPACE, &workspace)
        .env(HELPER_LOG, &missing_log)
        .env(HELPER_EXECUTED, &executed)
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = helper.status().expect("background error helper");
    assert!(status.success());
    let error = fs::read_to_string(result).expect("background error");
    assert!(error.starts_with(&format!(
        "cannot create background Run log {}:",
        missing_log.display()
    )));
    assert!(error.contains("No such file or directory"));
    assert!(!executed.exists(), "workload should not spawn");
}

#[test]
fn background_run_spawn_failure_is_typed() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let bin_directory = temporary_directory.path().join("bin");
    let workspace = temporary_directory.path().join("workspace");
    let log = temporary_directory.path().join("worker.log");
    let result = temporary_directory.path().join("result");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    fs::create_dir(&workspace).expect("Worker Workspace");
    write_executable(
        &bin_directory.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then rm \"$0\"; exit 0; fi\n",
    );
    let path = env::join_paths([
        bin_directory.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args(["--exact", "background_run_spawn_error_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_WORKSPACE, &workspace)
        .env(HELPER_LOG, &log)
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = helper.status().expect("background spawn error helper");
    assert!(status.success());
    let error = fs::read_to_string(result).expect("background spawn error");
    assert!(error.starts_with("cannot spawn claude background Run:"));
    assert!(error.contains("No such file or directory"));
}

#[test]
fn foreground_run_mirrors_terminal_streams_and_log() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let bin_directory = temporary_directory.path().join("bin");
    let workspace = temporary_directory.path().join("workspace");
    let log = temporary_directory.path().join("worker.log");
    let result = temporary_directory.path().join("result");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    fs::create_dir(&workspace).expect("Worker Workspace");
    write_executable(
        &bin_directory.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf 'stdout:%s\n' \"$(cat)\"\nprintf 'cwd=%s\n' \"$PWD\"\nprintf 'stderr-line\n' >&2\nexit 7\n",
    );
    let path = env::join_paths([
        bin_directory.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args(["--exact", "foreground_run_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_WORKSPACE, &workspace)
        .env(HELPER_LOG, &log)
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = helper.spawn().expect("foreground helper");
    child
        .stdin
        .take()
        .expect("helper stdin")
        .write_all(b"input-value\n")
        .expect("write helper input");
    let output = child.wait_with_output().expect("helper output");
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let terminal_stdout = String::from_utf8(output.stdout).expect("terminal stdout");
    assert!(terminal_stdout.contains("stdout:input-value\n"));
    assert!(terminal_stdout.contains(&format!(
        "cwd={}\n",
        workspace
            .canonicalize()
            .expect("canonical workspace")
            .display()
    )));
    let terminal_stderr = String::from_utf8(output.stderr).expect("terminal stderr");
    assert!(terminal_stderr.contains("stderr-line\n"));
    assert_eq!(
        fs::read_to_string(&result).expect("foreground result"),
        "exit=Some(7)"
    );
    let log_contents = fs::read_to_string(log).expect("mirrored log");
    assert!(log_contents.contains("stdout:input-value\n"));
    assert!(log_contents.contains("stderr-line\n"));
    assert!(log_contents.contains("cwd="));
}

#[test]
fn foreground_run_truncates_existing_log() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let bin_directory = temporary_directory.path().join("bin");
    let workspace = temporary_directory.path().join("workspace");
    let log = temporary_directory.path().join("worker.log");
    let result = temporary_directory.path().join("result");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    fs::create_dir(&workspace).expect("Worker Workspace");
    fs::write(&log, "stale output\n").expect("stale log");
    write_executable(
        &bin_directory.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf 'fresh output\n'\n",
    );
    let path = env::join_paths([
        bin_directory.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args([
            "--exact",
            "foreground_run_helper",
            "--ignored",
            "--nocapture",
        ])
        .env("PATH", path)
        .env(HELPER_WORKSPACE, &workspace)
        .env(HELPER_LOG, &log)
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = helper.status().expect("foreground helper");
    assert!(status.success());
    assert_eq!(
        fs::read_to_string(log).expect("foreground log"),
        "fresh output\n"
    );
}

#[test]
fn foreground_run_log_setup_failure_prevents_workload_spawn() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let bin_directory = temporary_directory.path().join("bin");
    let workspace = temporary_directory.path().join("workspace");
    let missing_log = temporary_directory
        .path()
        .join("missing")
        .join("worker.log");
    let executed = temporary_directory.path().join("executed");
    let result = temporary_directory.path().join("result");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    fs::create_dir(&workspace).expect("Worker Workspace");
    write_executable(
        &bin_directory.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\ntouch \"$WSG_AGENT_RUNTIME_HELPER_EXECUTED\"\n",
    );
    let path = env::join_paths([
        bin_directory.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args(["--exact", "foreground_run_error_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_WORKSPACE, &workspace)
        .env(HELPER_LOG, &missing_log)
        .env(HELPER_EXECUTED, &executed)
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = helper.status().expect("foreground error helper");
    assert!(status.success());
    let error = fs::read_to_string(result).expect("foreground error");
    assert!(error.starts_with(&format!(
        "cannot create foreground Run log {}:",
        missing_log.display()
    )));
    assert!(error.contains("No such file or directory"));
    assert!(!executed.exists(), "workload should not spawn");
}

#[test]
fn foreground_run_drains_large_stdout_and_stderr_concurrently() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let bin_directory = temporary_directory.path().join("bin");
    let workspace = temporary_directory.path().join("workspace");
    let log = temporary_directory.path().join("worker.log");
    let result = temporary_directory.path().join("result");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    fs::create_dir(&workspace).expect("Worker Workspace");
    write_executable(
        &bin_directory.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\ndd if=/dev/zero bs=1024 count=128 2>/dev/null | tr '\\0' 'o'\ndd if=/dev/zero bs=1024 count=128 2>/dev/null | tr '\\0' 'e' >&2\n",
    );
    let path = env::join_paths([
        bin_directory.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args(["--exact", "foreground_run_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_WORKSPACE, &workspace)
        .env(HELPER_LOG, &log)
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = helper.status().expect("foreground helper");
    assert!(status.success());
    assert_eq!(
        fs::read_to_string(result).expect("foreground result"),
        "exit=Some(0)"
    );
    let log_contents = fs::read(log).expect("large mirrored log");
    assert_eq!(
        log_contents.iter().filter(|byte| **byte == b'o').count(),
        128 * 1024
    );
    assert_eq!(
        log_contents.iter().filter(|byte| **byte == b'e').count(),
        128 * 1024
    );
}

#[test]
fn foreground_run_uses_the_same_supervisor_for_codex() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let bin_directory = temporary_directory.path().join("bin");
    let workspace = temporary_directory.path().join("workspace");
    let log = temporary_directory.path().join("worker.log");
    let result = temporary_directory.path().join("result");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    fs::create_dir(&workspace).expect("Worker Workspace");
    write_executable(
        &bin_directory.join("codex"),
        "#!/bin/sh\nif [ \"$1\" = \"features\" ]; then printf 'multi_agent stable false\n'; exit 0; fi\nprintf 'codex-output\n'\nprintf 'codex-error\n' >&2\n",
    );
    let path = env::join_paths([
        bin_directory.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args(["--exact", "foreground_run_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_RUNTIME, "codex")
        .env(HELPER_WORKSPACE, &workspace)
        .env(HELPER_LOG, &log)
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = helper.status().expect("foreground helper");
    assert!(status.success());
    assert_eq!(
        fs::read_to_string(result).expect("foreground result"),
        "exit=Some(0)"
    );
    let log_contents = fs::read_to_string(log).expect("Codex log");
    assert!(log_contents.contains("codex-output\n"));
    assert!(log_contents.contains("codex-error\n"));
}

#[test]
fn foreground_run_spawn_failure_is_typed() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let bin_directory = temporary_directory.path().join("bin");
    let workspace = temporary_directory.path().join("workspace");
    let log = temporary_directory.path().join("worker.log");
    let result = temporary_directory.path().join("result");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    fs::create_dir(&workspace).expect("Worker Workspace");
    write_executable(
        &bin_directory.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then rm \"$0\"; exit 0; fi\n",
    );
    let path = env::join_paths([
        bin_directory.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args(["--exact", "foreground_run_spawn_error_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_WORKSPACE, &workspace)
        .env(HELPER_LOG, &log)
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = helper.status().expect("foreground spawn error helper");
    assert!(status.success());
    let error = fs::read_to_string(result).expect("foreground spawn error");
    assert!(error.starts_with("cannot spawn claude foreground Run:"));
    assert!(error.contains("No such file or directory"));
}

#[test]
fn reserved_background_run_finalizes_worker_after_wait() {
    let temporary_directory = TempDir::new().expect("temporary directory");
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
    let growth = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("Worker Workspace should be provisioned");
    let worker_id = growth.added_workers()[0].clone();
    let bin_directory = temporary_directory.path().join("bin");
    let result = temporary_directory.path().join("result");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    write_executable(
        &bin_directory.join("claude"),
        concat!(
            "#!/bin/sh\n",
            "if [ \"$1\" = \"--help\" ]; then exit 0; fi\n",
            "printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\",\"total_cost_usd\":0.125}'\n",
            "exit 0\n",
        ),
    );
    let path = env::join_paths([
        bin_directory.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args([
            "--exact",
            "reserved_background_run_finalize_helper",
            "--ignored",
        ])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, temporary_directory.path())
        .env(HELPER_WORKER, worker_id.as_str())
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let output = helper
        .output()
        .expect("reserved finalization helper should run");
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(result).expect("reserved finalization result"),
        "exit=Some(0); conclusion=Succeeded; cost=Some(125000); source=Provider"
    );

    let snapshot = repository.worker_pool().snapshot();
    let worker = snapshot
        .worker(worker_id.as_str())
        .expect("finalized Worker");
    assert_eq!(worker.status(), WorkerStatus::Done);
    assert_eq!(worker.exit_code(), Some(0));
    assert!(worker.completed_at().is_some());
    assert!(worker.pid().is_some());
}

#[test]
fn reserved_background_run_finalizes_failed_worker_after_wait() {
    let temporary_directory = TempDir::new().expect("temporary directory");
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
    let growth = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("Worker Workspace should be provisioned");
    let worker_id = growth.added_workers()[0].clone();
    let bin_directory = temporary_directory.path().join("bin");
    let result = temporary_directory.path().join("result");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    write_executable(
        &bin_directory.join("claude"),
        concat!(
            "#!/bin/sh\n",
            "if [ \"$1\" = \"--help\" ]; then exit 0; fi\n",
            "printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"error_during_execution\",\"is_error\":true,\"result\":\"provider rejected the Run\"}'\n",
            "exit 0\n",
        ),
    );
    let path = env::join_paths([
        bin_directory.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args([
            "--exact",
            "reserved_background_run_finalize_helper",
            "--ignored",
        ])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, temporary_directory.path())
        .env(HELPER_WORKER, worker_id.as_str())
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let output = helper
        .output()
        .expect("reserved finalization helper should run");
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(result).expect("reserved finalization result"),
        "exit=Some(0); conclusion=Failed { message: \"provider rejected the Run\" }; cost=None; source=Provider"
    );

    let snapshot = repository.worker_pool().snapshot();
    let worker = snapshot
        .worker(worker_id.as_str())
        .expect("finalized Worker");
    assert_eq!(worker.status(), WorkerStatus::Failed);
    assert_eq!(worker.exit_code(), Some(1));
    assert!(worker.completed_at().is_some());
    assert_eq!(worker.error(), Some("provider rejected the Run"));
}

#[test]
fn reserved_foreground_run_finalizes_worker_after_completion() {
    let temporary_directory = TempDir::new().expect("temporary directory");
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
    let growth = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("Worker Workspace should be provisioned");
    let worker_id = growth.added_workers()[0].clone();
    let bin_directory = temporary_directory.path().join("bin");
    let result = temporary_directory.path().join("result");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    write_executable(
        &bin_directory.join("claude"),
        concat!(
            "#!/bin/sh\n",
            "if [ \"$1\" = \"--help\" ]; then exit 0; fi\n",
            "printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'\n",
            "exit 0\n",
        ),
    );
    let path = env::join_paths([
        bin_directory.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args([
            "--exact",
            "reserved_foreground_run_finalize_helper",
            "--ignored",
        ])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, temporary_directory.path())
        .env(HELPER_WORKER, worker_id.as_str())
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let output = helper
        .output()
        .expect("reserved foreground finalization helper should run");
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(result).expect("reserved foreground result"),
        "exit=Some(0)"
    );

    let snapshot = repository.worker_pool().snapshot();
    let worker = snapshot
        .worker(worker_id.as_str())
        .expect("finalized Worker");
    assert_eq!(worker.status(), WorkerStatus::Done);
    assert_eq!(worker.exit_code(), Some(0));
    assert!(worker.completed_at().is_some());
}

#[test]
#[ignore]
fn reserved_foreground_run_finalize_helper() {
    let repository = Repository::open(
        env::var_os(HELPER_REPOSITORY)
            .expect("repository")
            .as_os_str(),
    )
    .expect("repository");
    let worker_id =
        WorkerId::parse(env::var(HELPER_WORKER).expect("Worker ID")).expect("Worker ID");
    let reservation = repository
        .worker_pool()
        .reserve_named(worker_id, "ENG-215")
        .expect("Worker reservation");
    let result = env::var_os(HELPER_RESULT).expect("result path");
    let outcome = RunSupervisor::new()
        .run_reserved_foreground(reservation, AgentRuntimeInvocation::new("reserved test"))
        .expect("reserved foreground Run should complete");
    fs::write(result, format!("exit={:?}", outcome.exit_code())).expect("wait result");
}

#[test]
fn stale_background_waiter_cannot_finalize_newer_run() {
    let temporary_directory = TempDir::new().expect("temporary directory");
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
    let growth = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("Worker Workspace should be provisioned");
    let worker_id = growth.added_workers()[0].clone();
    let bin_directory = temporary_directory.path().join("bin");
    let process = temporary_directory.path().join("process");
    let reserved = temporary_directory.path().join("reserved");
    let release = temporary_directory.path().join("release");
    let result = temporary_directory.path().join("result");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    write_executable(
        &bin_directory.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nwhile [ ! -f \"$WSG_AGENT_RUNTIME_HELPER_RELEASE\" ]; do sleep 0.02; done\nexit 0\n",
    );
    let path = env::join_paths([
        bin_directory.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args(["--exact", "stale_background_waiter_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, temporary_directory.path())
        .env(HELPER_WORKER, worker_id.as_str())
        .env(HELPER_PROCESS, &process)
        .env(HELPER_RESERVED, &reserved)
        .env(HELPER_RELEASE, &release)
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let helper = BackgroundHelperGuard::spawn(&mut helper, &release, [&process, &result]);
    wait_for_file(&reserved);

    let worker_state = repository.state_store().worker(worker_id.clone());
    let loaded = match worker_state.load().expect("Run A Worker state") {
        wsg_core::Loaded::Present(versioned) => versioned,
        wsg_core::Loaded::Missing => panic!("Run A Worker state should exist"),
    };
    let revision = loaded.revision().clone();
    let mut state = loaded.value;
    state.status = WireStatus::new("idle");
    state.agent = None;
    state.ticket = None;
    state.pid = None;
    state.started_at = None;
    state.completed_at = None;
    state.log_file = None;
    state.branch_name = None;
    state.exit_code = None;
    state.error = None;
    let outcome = worker_state
        .commit(Expected::Match(revision), StateChange::Replace(state))
        .expect("reset Run A state");
    assert!(matches!(outcome, wsg_core::CommitOutcome::Applied(_)));
    repository
        .worker_pool()
        .reserve_named(worker_id.clone(), "ENG-217")
        .expect("Run B reservation");

    fs::write(&release, []).expect("release Run A");
    let output = helper.wait_with_output();
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(result).expect("Run A result"),
        "exit=Some(0)"
    );

    let snapshot = repository.worker_pool().snapshot();
    let worker = snapshot.worker(worker_id.as_str()).expect("newer Worker");
    assert_eq!(worker.status(), WorkerStatus::Busy);
    assert_eq!(worker.ticket(), Some("ENG-217"));
    assert_eq!(worker.pid(), None);
    assert_eq!(worker.completed_at(), None);
}

#[test]
#[ignore]
fn stale_background_waiter_helper() {
    let repository = Repository::open(
        env::var_os(HELPER_REPOSITORY)
            .expect("repository")
            .as_os_str(),
    )
    .expect("repository");
    let worker_id =
        WorkerId::parse(env::var(HELPER_WORKER).expect("Worker ID")).expect("Worker ID");
    let reservation = repository
        .worker_pool()
        .reserve_named(worker_id, "ENG-216")
        .expect("Run A reservation");
    let background = RunSupervisor::new()
        .run_reserved_background(reservation, AgentRuntimeInvocation::new("Run A"))
        .expect("Run A should start");
    fs::write(
        env::var_os(HELPER_PROCESS).expect("process path"),
        background.pid().to_string(),
    )
    .expect("Run A PID");
    fs::write(env::var_os(HELPER_RESERVED).expect("reserved path"), []).expect("Run A ready");
    wait_for_file(&PathBuf::from(
        env::var_os(HELPER_RELEASE).expect("release path"),
    ));
    let outcome = background.wait().expect("Run A should complete");
    fs::write(
        env::var_os(HELPER_RESULT).expect("result path"),
        format!("exit={:?}", outcome.exit_code()),
    )
    .expect("Run A result");
}

#[test]
#[ignore]
fn reserved_background_run_finalize_helper() {
    let repository = Repository::open(
        env::var_os(HELPER_REPOSITORY)
            .expect("repository")
            .as_os_str(),
    )
    .expect("repository");
    let worker_id =
        WorkerId::parse(env::var(HELPER_WORKER).expect("Worker ID")).expect("Worker ID");
    let reservation = repository
        .worker_pool()
        .reserve_named(worker_id, "ENG-214")
        .expect("Worker reservation");
    let result = env::var_os(HELPER_RESULT).expect("result path");
    let background = RunSupervisor::new()
        .run_reserved_background(reservation, AgentRuntimeInvocation::new("reserved test"))
        .expect("reserved background Run should start");
    let outcome = background.wait().expect("background Run should complete");
    fs::write(
        result,
        format!(
            "exit={:?}; conclusion={:?}; cost={:?}; source={:?}",
            outcome.exit_code(),
            outcome.result().conclusion(),
            outcome.result().cost().map(|cost| cost.as_micro_usd()),
            outcome.result_source(),
        ),
    )
    .expect("wait result");
}

#[test]
fn reset_run_terminates_the_run_process_group_and_returns_the_worker_to_idle() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let repository = initialize_repository(temporary_directory.path());
    let growth = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("Worker Workspace should be provisioned");
    let worker_id = growth.added_workers()[0].clone();

    let bin_directory = temporary_directory.path().join("bin");
    let result = temporary_directory.path().join("result");
    let process = temporary_directory.path().join("process");
    let descendant = temporary_directory.path().join("descendant");
    let diagnostic = temporary_directory.path().join("diagnostic");
    let exit = temporary_directory.path().join("exit");
    let release = temporary_directory.path().join("release");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    write_executable(&bin_directory.join("claude"), STUBBORN_RUNTIME);
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args(["--exact", "reset_run_helper", "--ignored"])
        .env("PATH", runtime_path(&bin_directory))
        .env(HELPER_REPOSITORY, temporary_directory.path())
        .env(HELPER_WORKER, worker_id.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_PROCESS, &process)
        .env(HELPER_DESCENDANT, &descendant)
        .env(HELPER_DIAGNOSTIC, &diagnostic)
        .env(HELPER_EXIT, &exit)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let helper = BackgroundHelperGuard::spawn(&mut helper, &release, [&process, &result]);

    wait_for_file(&result);
    wait_for_file(&diagnostic);
    wait_for_file(&descendant);
    let leader = read_pid(&result);
    let descendant_pid = read_pid(&descendant);

    let reset = RunSupervisor::new()
        .reset_run(&repository, &worker_id)
        .expect("Reset should abandon the Run");
    assert!(
        matches!(reset, RunReset::Abandoned { terminated_pid } if terminated_pid == Some(leader)),
        "unexpected Reset outcome: {reset:?}"
    );
    assert!(
        test_kill_process_group(unix_pid(leader)).is_err(),
        "Run process group should be gone once Reset returns"
    );
    assert!(
        test_kill_process(unix_pid(descendant_pid)).is_err(),
        "stubborn descendant should be gone once Reset returns"
    );

    let snapshot = repository.worker_pool().snapshot();
    let worker = snapshot.worker(worker_id.as_str()).expect("reset Worker");
    assert_eq!(worker.status(), WorkerStatus::Idle);
    assert_eq!(worker.pid(), None);
    assert_eq!(worker.ticket(), None);
    assert_eq!(worker.started_at(), None);
    assert_eq!(worker.completed_at(), None);
    assert_eq!(worker.log_file(), None);
    assert_eq!(worker.branch_name(), None);
    assert_eq!(worker.exit_code(), None);
    assert_eq!(worker.error(), None);

    let output = helper.wait_with_output();
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn reset_run_reports_an_already_idle_worker_without_changing_its_state() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let repository = initialize_repository(temporary_directory.path());
    let worker_id = grow_one_worker(&repository);
    let worker_path = worker_state_path(temporary_directory.path(), &worker_id);
    let before = fs::read(&worker_path).expect("idle Worker state");

    let reset = RunSupervisor::new()
        .reset_run(&repository, &worker_id)
        .expect("resetting an idle Worker should succeed");

    assert_eq!(reset, RunReset::AlreadyIdle);
    assert_eq!(
        fs::read(&worker_path).expect("idle Worker state"),
        before,
        "an idle Worker should not be rewritten"
    );
}

#[test]
fn reset_run_clears_a_terminal_run_and_preserves_unknown_fields_and_aliases() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let repository = initialize_repository(temporary_directory.path());
    let worker_id = grow_one_worker(&repository);
    let worker_path = worker_state_path(temporary_directory.path(), &worker_id);
    write_worker_json(
        &worker_path,
        serde_json::json!({
            "status": "failed",
            "agent": "codex",
            "ticket": "ENG-219",
            "pid": dead_pid(),
            "started_at": "2026-07-30T10:00:00Z",
            "completed_at": "2026-07-30T10:05:00Z",
            "log_file": "/repo/.jj/pool/worker.log",
            "branch_name": "eng-219",
            "exit_code": 1,
            "error": "tests failed",
            "future": { "enabled": true }
        }),
    );
    repository
        .worker_pool()
        .set_alias(worker_id.clone(), "reviewer")
        .expect("set Worker alias");

    let reset = RunSupervisor::new()
        .reset_run(&repository, &worker_id)
        .expect("resetting a terminal Run should succeed");

    assert_eq!(
        reset,
        RunReset::Abandoned {
            terminated_pid: None
        }
    );
    let snapshot = repository.worker_pool().snapshot();
    let worker = snapshot.worker(worker_id.as_str()).expect("reset Worker");
    assert_eq!(worker.status(), WorkerStatus::Idle);
    assert_eq!(worker.exit_code(), None);
    assert_eq!(worker.error(), None);
    assert_eq!(worker.completed_at(), None);
    assert_eq!(worker.branch_name(), None);
    assert_eq!(
        worker.alias(),
        "reviewer",
        "cosmetic pool metadata should survive a Reset"
    );

    let written = read_worker_json(&worker_path);
    assert_eq!(
        written["future"],
        serde_json::json!({ "enabled": true }),
        "unknown persisted fields should survive a Reset"
    );
    for field in [
        "agent",
        "ticket",
        "pid",
        "started_at",
        "completed_at",
        "log_file",
        "branch_name",
        "exit_code",
        "error",
    ] {
        assert_eq!(
            written[field],
            Value::Null,
            "{field} should be persisted as an explicit null"
        );
    }
}

#[test]
fn reset_run_clears_a_busy_run_whose_process_group_is_already_gone() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let repository = initialize_repository(temporary_directory.path());
    let worker_id = grow_one_worker(&repository);
    write_worker_json(
        &worker_state_path(temporary_directory.path(), &worker_id),
        serde_json::json!({
            "status": "busy",
            "agent": "claude",
            "ticket": "ENG-220",
            "pid": dead_pid(),
            "started_at": "2026-07-30T10:00:00Z",
            "completed_at": Value::Null,
            "log_file": "/repo/.jj/pool/worker.log",
            "branch_name": "eng-220",
            "exit_code": Value::Null,
            "error": Value::Null
        }),
    );

    let reset = RunSupervisor::new()
        .reset_run(&repository, &worker_id)
        .expect("resetting a dead busy Run should succeed");

    assert_eq!(
        reset,
        RunReset::Abandoned {
            terminated_pid: None
        }
    );
    let snapshot = repository.worker_pool().snapshot();
    let worker = snapshot.worker(worker_id.as_str()).expect("reset Worker");
    assert_eq!(worker.status(), WorkerStatus::Idle);
    assert_eq!(worker.pid(), None);
    assert_eq!(worker.ticket(), None);
}

#[test]
fn reset_run_reports_missing_worker_state() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let repository = initialize_repository(temporary_directory.path());
    let worker_id = grow_one_worker(&repository);
    fs::remove_file(worker_state_path(temporary_directory.path(), &worker_id))
        .expect("remove Worker state");

    let error = RunSupervisor::new()
        .reset_run(&repository, &worker_id)
        .expect_err("a Worker without state cannot be reset");

    assert!(
        matches!(error, RunSupervisorError::Reset { .. }),
        "unexpected Reset error: {error}"
    );
    assert!(error.to_string().contains("state is missing"), "{error}");
}

#[test]
fn reset_run_leaves_malformed_worker_state_untouched() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let repository = initialize_repository(temporary_directory.path());
    let worker_id = grow_one_worker(&repository);
    let worker_path = worker_state_path(temporary_directory.path(), &worker_id);
    fs::write(&worker_path, b"{ not json").expect("malformed Worker state");

    let error = RunSupervisor::new()
        .reset_run(&repository, &worker_id)
        .expect_err("unreadable Worker state cannot be reset");

    assert!(
        matches!(error, RunSupervisorError::Reset { .. }),
        "unexpected Reset error: {error}"
    );
    assert_eq!(
        fs::read(&worker_path).expect("malformed Worker state"),
        b"{ not json",
        "a Reset must not overwrite state it could not read"
    );
}

#[test]
fn reset_run_retries_when_its_own_run_finalizes_during_process_cleanup() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let repository = initialize_repository(temporary_directory.path());
    let worker_id = grow_one_worker(&repository);
    let signals = StubbornRun::launch(temporary_directory.path(), &repository, &worker_id);

    let reset = thread::scope(|scope| {
        let resetting = scope.spawn(|| RunSupervisor::new().reset_run(&repository, &worker_id));
        signals.wait_for_termination_signal();
        finalize_run_in_place(&repository, &worker_id);
        resetting.join().expect("Reset thread")
    })
    .expect("Reset should retry past its own Run's finalization");

    assert_eq!(
        reset,
        RunReset::Abandoned {
            terminated_pid: Some(signals.leader())
        }
    );
    let snapshot = repository.worker_pool().snapshot();
    let worker = snapshot.worker(worker_id.as_str()).expect("reset Worker");
    assert_eq!(
        worker.status(),
        WorkerStatus::Idle,
        "a Run that finalized mid-Reset must still leave the Worker idle"
    );
    assert_eq!(worker.pid(), None);
    assert_eq!(worker.ticket(), None);
    assert_eq!(worker.exit_code(), None);
    signals.expect_clean_shutdown();
}

#[test]
fn reset_run_cannot_clear_a_newer_run_reserved_during_process_cleanup() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let repository = initialize_repository(temporary_directory.path());
    let worker_id = grow_one_worker(&repository);
    let signals = StubbornRun::launch(temporary_directory.path(), &repository, &worker_id);

    let reset = thread::scope(|scope| {
        let resetting = scope.spawn(|| RunSupervisor::new().reset_run(&repository, &worker_id));
        signals.wait_for_termination_signal();
        release_run_in_place(&repository, &worker_id);
        repository
            .worker_pool()
            .reserve_named(worker_id.clone(), "ENG-999")
            .expect("newer Run reservation");
        resetting.join().expect("Reset thread")
    })
    .expect("Reset should report a superseded Run rather than fail");

    assert_eq!(reset, RunReset::Superseded);
    let snapshot = repository.worker_pool().snapshot();
    let worker = snapshot.worker(worker_id.as_str()).expect("newer Worker");
    assert_eq!(
        worker.status(),
        WorkerStatus::Busy,
        "a superseded Reset must not clear the newer Run"
    );
    assert_eq!(worker.ticket(), Some("ENG-999"));
    assert_eq!(worker.pid(), None);
    assert_eq!(worker.completed_at(), None);
    signals.expect_clean_shutdown();

    let snapshot = repository.worker_pool().snapshot();
    let worker = snapshot.worker(worker_id.as_str()).expect("newer Worker");
    assert_eq!(
        worker.status(),
        WorkerStatus::Busy,
        "the abandoned Run's waiter must not finalize the newer Run"
    );
    assert_eq!(worker.ticket(), Some("ENG-999"));
}

#[test]
fn concurrent_resets_return_one_worker_to_idle_exactly_once() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let repository = initialize_repository(temporary_directory.path());
    let worker_id = grow_one_worker(&repository);
    write_worker_json(
        &worker_state_path(temporary_directory.path(), &worker_id),
        busy_run_state(dead_pid(), "ENG-221"),
    );

    let outcomes = thread::scope(|scope| {
        let first = scope.spawn(|| RunSupervisor::new().reset_run(&repository, &worker_id));
        let second = scope.spawn(|| RunSupervisor::new().reset_run(&repository, &worker_id));
        [
            first.join().expect("first Reset thread"),
            second.join().expect("second Reset thread"),
        ]
    });

    let mut abandoned = 0;
    let mut already_idle = 0;
    for outcome in outcomes {
        match outcome.expect("concurrent Resets should both succeed") {
            RunReset::Abandoned { .. } => abandoned += 1,
            RunReset::AlreadyIdle => already_idle += 1,
            RunReset::Superseded => panic!("a Reset of the same Run cannot be superseded"),
        }
    }
    assert_eq!(abandoned, 1, "exactly one Reset should abandon the Run");
    assert_eq!(
        already_idle, 1,
        "the losing Reset should observe an idle Worker"
    );

    let snapshot = repository.worker_pool().snapshot();
    let worker = snapshot.worker(worker_id.as_str()).expect("reset Worker");
    assert_eq!(worker.status(), WorkerStatus::Idle);
    assert_eq!(worker.pid(), None);
}

#[test]
fn reset_run_and_dead_pid_reconciliation_agree_on_an_idle_worker() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let repository = initialize_repository(temporary_directory.path());
    let worker_id = grow_one_worker(&repository);
    write_worker_json(
        &worker_state_path(temporary_directory.path(), &worker_id),
        busy_run_state(dead_pid(), "ENG-222"),
    );

    let reset = thread::scope(|scope| {
        let resetting = scope.spawn(|| RunSupervisor::new().reset_run(&repository, &worker_id));
        let reconciling = scope.spawn(|| repository.worker_pool().reconcile_runs());
        let reconciled = reconciling.join().expect("reconciliation thread");
        assert!(
            reconciled.diagnostics().is_empty(),
            "reconciliation should not report a conflict as a diagnostic: {:?}",
            reconciled.diagnostics()
        );
        resetting.join().expect("Reset thread")
    })
    .expect("Reset should succeed alongside reconciliation");

    assert!(
        matches!(reset, RunReset::Abandoned { .. } | RunReset::AlreadyIdle),
        "unexpected Reset outcome: {reset:?}"
    );
    let snapshot = repository.worker_pool().snapshot();
    let worker = snapshot.worker(worker_id.as_str()).expect("reset Worker");
    assert_eq!(
        worker.status(),
        WorkerStatus::Idle,
        "reconciliation must not resurrect a Run that Reset abandoned"
    );
    assert_eq!(worker.pid(), None);
    assert_eq!(worker.error(), None);
}

#[test]
#[ignore]
fn reset_run_helper() {
    let repository = Repository::open(
        env::var_os(HELPER_REPOSITORY)
            .expect("repository")
            .as_os_str(),
    )
    .expect("repository");
    let worker_id =
        WorkerId::parse(env::var(HELPER_WORKER).expect("Worker ID")).expect("Worker ID");
    let reservation = repository
        .worker_pool()
        .reserve_named(worker_id, "ENG-218")
        .expect("Worker reservation");
    let background = RunSupervisor::new()
        .run_reserved_background(reservation, AgentRuntimeInvocation::new("reset test"))
        .expect("reserved background Run should start");
    fs::write(
        env::var_os(HELPER_RESULT).expect("result path"),
        background.pid().to_string(),
    )
    .expect("launched PID");
    let outcome = background.wait().expect("abandoned Run should be reaped");
    fs::write(
        env::var_os(HELPER_EXIT).expect("exit path"),
        format!("exit={:?}", outcome.exit_code()),
    )
    .expect("abandoned Run outcome");
}

#[test]
#[ignore]
fn reserved_background_run_helper() {
    let repository = Repository::open(
        env::var_os(HELPER_REPOSITORY)
            .expect("repository")
            .as_os_str(),
    )
    .expect("repository");
    let worker_id =
        WorkerId::parse(env::var(HELPER_WORKER).expect("Worker ID")).expect("Worker ID");
    let reservation = repository
        .worker_pool()
        .reserve_named(worker_id, "ENG-213")
        .expect("Worker reservation");
    if let Some(reserved) = env::var_os(HELPER_RESERVED) {
        fs::write(reserved, []).expect("reservation barrier");
        wait_for_file(&PathBuf::from(
            env::var_os(HELPER_PROCEED).expect("proceed barrier"),
        ));
    }
    let result = env::var_os(HELPER_RESULT).expect("result path");
    match RunSupervisor::new()
        .run_reserved_background(reservation, AgentRuntimeInvocation::new("reserved test"))
    {
        Ok(background) => {
            fs::write(&result, background.pid().to_string()).expect("background PID result");
            let outcome = background.wait().expect("background Run should complete");
            assert_eq!(outcome.exit_code(), Some(0));
        }
        Err(error) => {
            if let RunSupervisorError::PersistPid { pid, .. }
            | RunSupervisorError::PersistPidMissing { pid, .. } = error
            {
                fs::write(
                    env::var_os(HELPER_PROCESS).expect("process path"),
                    pid.to_string(),
                )
                .expect("persist failed PID");
            } else if let RunSupervisorError::ReservationRelease { pid: Some(pid), .. } = &error {
                fs::write(
                    env::var_os(HELPER_PROCESS).expect("process path"),
                    pid.to_string(),
                )
                .expect("persist failed PID");
            }
            fs::write(result, error.to_string()).expect("persistence error result");
        }
    }
}

#[test]
#[ignore]
fn background_run_helper() {
    let workspace = PathBuf::from(env::var_os(HELPER_WORKSPACE).expect("Worker Workspace"));
    let log = PathBuf::from(env::var_os(HELPER_LOG).expect("log path"));
    let result = PathBuf::from(env::var_os(HELPER_RESULT).expect("result path"));
    let exit = PathBuf::from(env::var_os(HELPER_EXIT).expect("exit path"));
    let request = RunRequest::new(
        AgentRuntime::Claude,
        AgentRuntimeInvocation::new("background test"),
        workspace,
        log,
    );
    let background = RunSupervisor::new()
        .run_background(&request)
        .expect("background Run should start");
    fs::write(result, background.pid().to_string()).expect("background PID result");
    let outcome = background.wait().expect("background Run should complete");
    fs::write(exit, format!("exit={:?}", outcome.exit_code())).expect("background exit result");
}

#[test]
#[ignore]
fn background_run_error_helper() {
    let workspace = PathBuf::from(env::var_os(HELPER_WORKSPACE).expect("Worker Workspace"));
    let log = PathBuf::from(env::var_os(HELPER_LOG).expect("log path"));
    let result = PathBuf::from(env::var_os(HELPER_RESULT).expect("result path"));
    let request = RunRequest::new(
        AgentRuntime::Claude,
        AgentRuntimeInvocation::new("background error test"),
        workspace,
        log,
    );
    let error = RunSupervisor::new()
        .run_background(&request)
        .expect_err("log setup should fail");
    fs::write(result, error.to_string()).expect("background error result");
}

#[test]
#[ignore]
fn background_run_spawn_error_helper() {
    let workspace = PathBuf::from(env::var_os(HELPER_WORKSPACE).expect("Worker Workspace"));
    let log = PathBuf::from(env::var_os(HELPER_LOG).expect("log path"));
    let result = PathBuf::from(env::var_os(HELPER_RESULT).expect("result path"));
    let request = RunRequest::new(
        AgentRuntime::Claude,
        AgentRuntimeInvocation::new("background spawn error test"),
        workspace,
        log,
    );
    let error = RunSupervisor::new()
        .run_background(&request)
        .expect_err("spawn should fail after probe");
    fs::write(result, error.to_string()).expect("background spawn error result");
}

#[test]
#[ignore]
fn foreground_run_spawn_error_helper() {
    let workspace = PathBuf::from(env::var_os(HELPER_WORKSPACE).expect("Worker Workspace"));
    let log = PathBuf::from(env::var_os(HELPER_LOG).expect("log path"));
    let result = PathBuf::from(env::var_os(HELPER_RESULT).expect("result path"));
    let request = RunRequest::new(
        AgentRuntime::Claude,
        AgentRuntimeInvocation::new("foreground spawn error test"),
        workspace,
        log,
    );
    let error = RunSupervisor::new()
        .run_foreground(&request)
        .expect_err("spawn should fail after probe");
    fs::write(result, error.to_string()).expect("foreground spawn error result");
}

#[test]
#[ignore]
fn foreground_run_error_helper() {
    let workspace = PathBuf::from(env::var_os(HELPER_WORKSPACE).expect("Worker Workspace"));
    let log = PathBuf::from(env::var_os(HELPER_LOG).expect("log path"));
    let result = PathBuf::from(env::var_os(HELPER_RESULT).expect("result path"));
    let request = RunRequest::new(
        AgentRuntime::Claude,
        AgentRuntimeInvocation::new("foreground error test"),
        workspace,
        log,
    );
    let error = RunSupervisor::new()
        .run_foreground(&request)
        .expect_err("log setup should fail");
    fs::write(result, error.to_string()).expect("foreground error result");
}

#[test]
#[ignore]
fn foreground_run_helper() {
    let workspace = PathBuf::from(env::var_os(HELPER_WORKSPACE).expect("Worker Workspace"));
    let log = PathBuf::from(env::var_os(HELPER_LOG).expect("log path"));
    let result = PathBuf::from(env::var_os(HELPER_RESULT).expect("result path"));
    let runtime = match env::var(HELPER_RUNTIME).as_deref() {
        Ok("codex") => AgentRuntime::Codex,
        Ok("claude") | Err(_) => AgentRuntime::Claude,
        Ok(other) => panic!("unknown Agent Runtime {other}"),
    };
    let request = RunRequest::new(
        runtime,
        AgentRuntimeInvocation::new("foreground test"),
        workspace,
        log,
    );
    let outcome = RunSupervisor::new()
        .run_foreground(&request)
        .expect("foreground Run should complete");
    fs::write(result, format!("exit={:?}", outcome.exit_code())).expect("foreground result");
}

#[test]
#[ignore]
fn agent_runtime_probe_helper() {
    let mode = env::var(HELPER_MODE).expect("helper mode");
    let workspace = env::current_dir().expect("helper workspace");
    let path = PathBuf::from(env::var_os(HELPER_PATH).expect("helper PATH"));
    let result = PathBuf::from(env::var_os(HELPER_RESULT).expect("helper result"));
    let contents = match mode.as_str() {
        "missing" => {
            let error = AgentRuntime::Claude
                .probe(&workspace)
                .expect_err("missing runtime should fail");
            assert!(matches!(
                error,
                AgentRuntimeProbeError::ExecutableNotFound { .. }
            ));
            error.to_string()
        }
        "claude" | "codex" | "failed" | "workspace" => {
            let runtime = if mode == "codex" {
                AgentRuntime::Codex
            } else {
                AgentRuntime::Claude
            };
            let capabilities = runtime
                .probe(&workspace)
                .expect("runtime probe should start");
            format!(
                "forward={} multi={}",
                capabilities.forward_subagent_text(),
                capabilities.multi_agent()
            )
        }
        _ => panic!("unknown helper mode {mode}"),
    };
    fs::write(result, contents).expect("write probe result");
    assert!(path.is_dir(), "helper PATH should be a directory");
}

struct BackgroundHelperGuard {
    child: Option<Child>,
    release: PathBuf,
    process_ids: [PathBuf; 2],
}

impl BackgroundHelperGuard {
    fn spawn(
        command: &mut Command,
        release: &std::path::Path,
        process_ids: [&std::path::Path; 2],
    ) -> Self {
        Self {
            child: Some(command.spawn().expect("background helper")),
            release: release.to_owned(),
            process_ids: process_ids.map(std::path::Path::to_owned),
        }
    }

    fn is_running(&mut self) -> bool {
        self.child
            .as_mut()
            .expect("background helper")
            .try_wait()
            .expect("background helper status")
            .is_none()
    }

    fn wait_with_output(mut self) -> std::process::Output {
        self.child
            .take()
            .expect("background helper")
            .wait_with_output()
            .expect("background helper output")
    }
}

impl Drop for BackgroundHelperGuard {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = fs::write(&self.release, []);
        for path in &self.process_ids {
            let Some(pid) = fs::read_to_string(path)
                .ok()
                .and_then(|contents| contents.split_whitespace().next()?.parse::<i32>().ok())
                .and_then(Pid::from_raw)
            else {
                continue;
            };
            let _ = kill_process_group(pid, Signal::KILL);
            break;
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// A stubborn runtime whose leader records the TERM it ignores, so a test can
/// act inside the Reset grace window before the forced kill lands.
const TERM_REPORTING_RUNTIME: &str = concat!(
    "#!/bin/bash\n",
    "if [ \"$1\" = \"--help\" ]; then exit 0; fi\n",
    "( trap '' TERM; while :; do sleep 0.05; done ) &\n",
    "printf '%s\\n' \"$!\" > \"$WSG_AGENT_RUNTIME_HELPER_DESCENDANT\"\n",
    "trap 'printf term > \"$WSG_AGENT_RUNTIME_HELPER_TERM\"' TERM\n",
    "printf '%s\\n' \"$$\" > \"$WSG_AGENT_RUNTIME_HELPER_PROCESS\"\n",
    "printf started > \"$WSG_AGENT_RUNTIME_HELPER_DIAGNOSTIC\"\n",
    "while :; do sleep 0.05; done\n",
);

/// One stubborn background Run launched by a separate process, so the leader is
/// reaped by its own launcher exactly as it is in production.
struct StubbornRun {
    helper: BackgroundHelperGuard,
    leader: u32,
    term: PathBuf,
}

impl StubbornRun {
    fn launch(root: &std::path::Path, repository: &Repository, worker: &WorkerId) -> Self {
        let bin_directory = root.join("bin");
        let result = root.join("result");
        let process = root.join("process");
        let descendant = root.join("descendant");
        let diagnostic = root.join("diagnostic");
        let exit = root.join("exit");
        let release = root.join("release");
        let term = root.join("term");
        fs::create_dir(&bin_directory).expect("runtime bin directory");
        write_executable(&bin_directory.join("claude"), TERM_REPORTING_RUNTIME);
        let mut helper = Command::new(env::current_exe().expect("test executable"));
        helper
            .args(["--exact", "reset_run_helper", "--ignored"])
            .env("PATH", runtime_path(&bin_directory))
            .env(HELPER_REPOSITORY, root)
            .env(HELPER_WORKER, worker.as_str())
            .env(HELPER_RESULT, &result)
            .env(HELPER_PROCESS, &process)
            .env(HELPER_DESCENDANT, &descendant)
            .env(HELPER_DIAGNOSTIC, &diagnostic)
            .env(HELPER_TERM, &term)
            .env(HELPER_EXIT, &exit)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let helper = BackgroundHelperGuard::spawn(&mut helper, &release, [&process, &result]);
        wait_for_file(&result);
        wait_for_file(&diagnostic);
        wait_for_file(&descendant);
        let leader = read_pid(&result);
        assert!(
            matches!(
                repository.worker_pool().snapshot().worker(worker.as_str()),
                Some(worker) if worker.status() == WorkerStatus::Busy
            ),
            "the launched Run should be busy before the race starts"
        );
        Self {
            helper,
            leader,
            term,
        }
    }

    fn leader(&self) -> u32 {
        self.leader
    }

    /// Blocks until the Run has received TERM, proving the caller is acting
    /// inside the Reset grace window rather than before or after cleanup.
    fn wait_for_termination_signal(&self) {
        wait_for_file(&self.term);
    }

    fn expect_clean_shutdown(self) {
        assert!(
            test_kill_process_group(unix_pid(self.leader)).is_err(),
            "Run process group should be gone once Reset returns"
        );
        let output = self.helper.wait_with_output();
        assert!(
            output.status.success(),
            "helper failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn busy_run_state(pid: u32, ticket: &str) -> Value {
    serde_json::json!({
        "status": "busy",
        "agent": "claude",
        "ticket": ticket,
        "pid": pid,
        "started_at": "2026-07-30T10:00:00Z",
        "completed_at": Value::Null,
        "log_file": "/repo/.jj/pool/worker.log",
        "branch_name": ticket.to_lowercase(),
        "exit_code": Value::Null,
        "error": Value::Null
    })
}

/// Marks the current Run done exactly as its own waiter would, keeping every
/// Run identity field so a concurrent Reset still recognizes its own Run.
fn finalize_run_in_place(repository: &Repository, worker: &WorkerId) {
    update_worker_state(repository, worker, |state| {
        state.status = WireStatus::new("done");
        state.completed_at = Some(wsg_core::WireTimestamp::new("2026-07-30T10:05:00Z"));
        state.exit_code = Some(0);
    });
}

/// Returns the Worker to idle so the test can reserve a newer Run on it.
fn release_run_in_place(repository: &Repository, worker: &WorkerId) {
    update_worker_state(repository, worker, |state| {
        state.status = WireStatus::new("idle");
        state.agent = None;
        state.ticket = None;
        state.pid = None;
        state.started_at = None;
        state.completed_at = None;
        state.log_file = None;
        state.branch_name = None;
        state.exit_code = None;
        state.error = None;
    });
}

fn update_worker_state(
    repository: &Repository,
    worker: &WorkerId,
    change: impl FnOnce(&mut wsg_core::WorkerState),
) {
    let state_repository = repository.state_store().worker(worker.clone());
    let loaded = match state_repository.load().expect("Worker state") {
        wsg_core::Loaded::Present(versioned) => versioned,
        wsg_core::Loaded::Missing => panic!("Worker state should exist"),
    };
    let (mut state, revision) = loaded.into_parts();
    change(&mut state);
    let outcome = state_repository
        .commit(Expected::Match(revision), StateChange::Replace(state))
        .expect("commit Worker state");
    assert!(
        matches!(outcome, wsg_core::CommitOutcome::Applied(_)),
        "the test's own Worker mutation should apply"
    );
}

/// A runtime whose leader and descendant both ignore TERM, so only a forced
/// process-group kill can end the Run.
const STUBBORN_RUNTIME: &str = concat!(
    "#!/bin/bash\n",
    "if [ \"$1\" = \"--help\" ]; then exit 0; fi\n",
    "( trap '' TERM; while :; do sleep 0.05; done ) &\n",
    "printf '%s\\n' \"$!\" > \"$WSG_AGENT_RUNTIME_HELPER_DESCENDANT\"\n",
    "trap '' TERM\n",
    "printf '%s\\n' \"$$\" > \"$WSG_AGENT_RUNTIME_HELPER_PROCESS\"\n",
    "printf started > \"$WSG_AGENT_RUNTIME_HELPER_DIAGNOSTIC\"\n",
    "while :; do sleep 0.05; done\n",
);

fn grow_one_worker(repository: &Repository) -> WorkerId {
    repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("Worker Workspace should be provisioned")
        .added_workers()[0]
        .clone()
}

fn worker_state_path(root: &std::path::Path, worker: &WorkerId) -> PathBuf {
    root.join(".jj/pool").join(format!("{worker}.json"))
}

fn write_worker_json(path: &std::path::Path, state: Value) {
    fs::write(
        path,
        serde_json::to_vec_pretty(&state).expect("Worker JSON"),
    )
    .expect("Worker state");
}

fn read_worker_json(path: &std::path::Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("Worker state")).expect("Worker JSON")
}

/// Returns a process identifier that has been started and reaped, so the
/// kernel reports it as absent without racing an unrelated live process.
fn dead_pid() -> u32 {
    let mut exited = Command::new("sh")
        .args(["-c", "exit 0"])
        .spawn()
        .expect("dead PID helper");
    let pid = exited.id();
    exited.wait().expect("dead PID helper should exit");
    pid
}

fn initialize_repository(root: &std::path::Path) -> Repository {
    let output = Command::new("jj")
        .args(["--config", "signing.behavior=drop", "git", "init"])
        .arg(root)
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
        .current_dir(root)
        .output()
        .expect("jj remote add should run");
    assert!(
        output.status.success(),
        "jj remote add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Repository::open(root).expect("repository")
}

fn runtime_path(bin_directory: &std::path::Path) -> std::ffi::OsString {
    env::join_paths([
        bin_directory.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH")
}

fn read_pid(path: &std::path::Path) -> u32 {
    fs::read_to_string(path)
        .expect("process identity")
        .split_whitespace()
        .next()
        .expect("process identity value")
        .parse()
        .expect("numeric process identity")
}

fn unix_pid(pid: u32) -> Pid {
    Pid::from_raw(i32::try_from(pid).expect("process identity fits a Unix process ID"))
        .expect("Unix process ID")
}

fn wait_for_file(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !path.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(path.exists(), "{} should be created", path.display());
}

fn helper_command(path: &std::path::Path, result: &std::path::Path, mode: &str) -> Command {
    let mut command = Command::new(env::current_exe().expect("test executable"));
    command
        .args(["--exact", "agent_runtime_probe_helper", "--ignored"])
        .env(HELPER_MODE, mode)
        .env(HELPER_PATH, path)
        .env(HELPER_RESULT, result)
        .env(HELPER_WORKSPACE, path.join("workspace-result"))
        .env("PATH", path)
        .current_dir(path);
    command
}

fn command_args(command: &Command) -> Vec<String> {
    command
        .get_args()
        .map(|argument| {
            argument
                .to_str()
                .expect("command argument should be UTF-8")
                .to_owned()
        })
        .collect()
}

fn write_executable(path: &std::path::Path, content: &str) {
    fs::write(path, content).expect("fake runtime executable");
    let mut permissions = fs::metadata(path)
        .expect("fake runtime metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make fake runtime executable");
}
