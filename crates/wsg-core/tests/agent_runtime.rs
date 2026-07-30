use std::env;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{kill_process_group, test_kill_process, Pid, Signal};
use tempfile::TempDir;
use wsg_core::{
    AgentRuntime, AgentRuntimeCapabilities, AgentRuntimeInvocation, AgentRuntimeProbeError,
    Expected, PoolCapacity, Repository, RunRequest, RunSupervisor, RunSupervisorError, StateChange,
    WireStatus, WorkerId, WorkerStatus,
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

#[test]
fn fresh_claude_command_preserves_headless_stream_invocation() {
    let invocation = AgentRuntimeInvocation::new("implement the thing")
        .with_model("opus")
        .with_name("pool:worker-abc:AMBA-42")
        .with_system_prompt("dispatch rules");
    let command =
        AgentRuntime::Claude.command(&invocation, AgentRuntimeCapabilities::new(false, true));

    assert_eq!(command.get_program(), "claude");
    assert_eq!(
        command_args(&command),
        vec![
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
            "dispatch rules",
            "implement the thing",
        ]
    );
}

#[test]
fn resumed_claude_command_does_not_repeat_system_prompt() {
    let invocation = AgentRuntimeInvocation::new("fix the tests")
        .with_model("opus")
        .with_session_id("sess-abc-123")
        .with_system_prompt("must not be repeated");
    let command = AgentRuntime::Claude.command(&invocation, AgentRuntimeCapabilities::default());

    assert_eq!(
        command_args(&command),
        vec![
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
            "fix the tests",
        ]
    );
}

#[test]
fn fresh_codex_command_preserves_workspace_dispatch_invocation() {
    let invocation = AgentRuntimeInvocation::new("implement it")
        .with_model("gpt-test")
        .with_system_prompt("system rules");
    let command =
        AgentRuntime::Codex.command(&invocation, AgentRuntimeCapabilities::new(true, false));

    assert_eq!(command.get_program(), "codex");
    assert_eq!(
        command_args(&command),
        vec![
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
            "system rules\n\nimplement it",
        ]
    );
}

#[test]
fn resumed_codex_command_does_not_repeat_system_prompt() {
    let invocation = AgentRuntimeInvocation::new("continue")
        .with_model("gpt-test")
        .with_session_id("thread-123")
        .with_system_prompt("must not be repeated");
    let command =
        AgentRuntime::Codex.command(&invocation, AgentRuntimeCapabilities::new(true, false));

    assert_eq!(
        command_args(&command),
        vec![
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
            "continue",
        ]
    );
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

    assert!(!command_args(&claude)
        .iter()
        .any(|arg| arg == "--forward-subagent-text"));
    assert!(!command_args(&claude).iter().any(|arg| arg == "multi_agent"));
    assert!(!command_args(&codex)
        .iter()
        .any(|arg| arg == "--forward-subagent-text"));
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
        .grow_to(PoolCapacity::new(1).expect("capacity"))
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
        .grow_to(PoolCapacity::new(1).expect("capacity"))
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
        .grow_to(PoolCapacity::new(1).expect("capacity"))
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
        .grow_to(PoolCapacity::new(1).expect("capacity"))
        .expect("Worker Workspace should be provisioned");
    let worker_id = growth.added_workers()[0].clone();
    let bin_directory = temporary_directory.path().join("bin");
    let result = temporary_directory.path().join("result");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    write_executable(
        &bin_directory.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nexit 0\n",
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
        "exit=Some(0)"
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
        .grow_to(PoolCapacity::new(1).expect("capacity"))
        .expect("Worker Workspace should be provisioned");
    let worker_id = growth.added_workers()[0].clone();
    let bin_directory = temporary_directory.path().join("bin");
    let result = temporary_directory.path().join("result");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    write_executable(
        &bin_directory.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nexit 7\n",
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
        "exit=Some(7)"
    );

    let snapshot = repository.worker_pool().snapshot();
    let worker = snapshot
        .worker(worker_id.as_str())
        .expect("finalized Worker");
    assert_eq!(worker.status(), WorkerStatus::Failed);
    assert_eq!(worker.exit_code(), Some(7));
    assert!(worker.completed_at().is_some());
    assert!(worker.error().is_some());
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
        .grow_to(PoolCapacity::new(1).expect("capacity"))
        .expect("Worker Workspace should be provisioned");
    let worker_id = growth.added_workers()[0].clone();
    let bin_directory = temporary_directory.path().join("bin");
    let result = temporary_directory.path().join("result");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    write_executable(
        &bin_directory.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nexit 0\n",
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
        .run_reserved_foreground(&reservation, AgentRuntimeInvocation::new("reserved test"))
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
        .grow_to(PoolCapacity::new(1).expect("capacity"))
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
        .run_reserved_background(&reservation, AgentRuntimeInvocation::new("Run A"))
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
        .run_reserved_background(&reservation, AgentRuntimeInvocation::new("reserved test"))
        .expect("reserved background Run should start");
    let outcome = background.wait().expect("background Run should complete");
    fs::write(result, format!("exit={:?}", outcome.exit_code())).expect("wait result");
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
        .run_reserved_background(&reservation, AgentRuntimeInvocation::new("reserved test"))
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
