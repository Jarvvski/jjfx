use std::env;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process_group};
use tempfile::TempDir;
use wsg_core::{
    AgentRuntime, AgentRuntimeCapabilities, AgentRuntimeInvocation, AgentRuntimeProbeError,
    RunRequest, RunSupervisor,
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
