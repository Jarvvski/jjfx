use std::env;
use std::fs;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use jjfx::engine::{
    AgentModel, AgentRuntime, AgentRuntimeCapabilities, AgentRuntimeInvocation,
    AgentRuntimeProbeError, Expected, PoolCapacity, Repository, RunRequest, RunReset,
    RunSupervisor, RunSupervisorError, StateChange, WireAgent, WireStatus, WorkerId, WorkerStatus,
};
use rustix::process::{
    Pid, Signal, getpgid, kill_process, kill_process_group, test_kill_process,
    test_kill_process_group,
};
use serde_json::Value;
use tempfile::TempDir;

mod support;

use support::HELPER_LIVENESS_TIMEOUT;

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
const HELPER_PTY_SLAVE: &str = "WSG_AGENT_RUNTIME_HELPER_PTY_SLAVE";
const HELPER_OPENCODE_WORKSPACE: &str = "WSG_AGENT_RUNTIME_HELPER_OPENCODE_WORKSPACE";
const HELPER_OPENCODE_GET_DIRECTORY: &str = "WSG_AGENT_RUNTIME_HELPER_OPENCODE_GET_DIRECTORY";
const HELPER_OPENCODE_SESSION_ID: &str = "WSG_AGENT_RUNTIME_HELPER_OPENCODE_SESSION_ID";
const HELPER_OPENCODE_ACTION: &str = "WSG_AGENT_RUNTIME_HELPER_OPENCODE_ACTION";
const HELPER_OPENCODE_INTERRUPT: &str = "WSG_AGENT_RUNTIME_HELPER_OPENCODE_INTERRUPT";
const HELPER_OPENCODE_UNAVAILABLE: &str = "WSG_AGENT_RUNTIME_HELPER_OPENCODE_UNAVAILABLE";
const HELPER_OPENCODE_OUTCOME: &str = "WSG_AGENT_RUNTIME_HELPER_OPENCODE_OUTCOME";

#[test]
fn pi_runtime_has_a_canonical_configured_and_persisted_identity() {
    let configured = AgentRuntime::from_configured(Some(&WireAgent::new(" PI ")))
        .expect("Pi should be a supported configured runtime");

    assert_eq!(configured, AgentRuntime::Pi);
    assert_eq!(configured.as_str(), "pi");
}

#[test]
fn legacy_missing_and_blank_runtime_configuration_still_defaults_to_claude() {
    assert_eq!(
        AgentRuntime::from_configured(None).expect("missing runtime should default"),
        AgentRuntime::Claude
    );
    assert_eq!(
        AgentRuntime::from_configured(Some(&WireAgent::new("  ")))
            .expect("blank runtime should default"),
        AgentRuntime::Claude
    );
}

#[test]
fn pi_model_selection_keeps_provider_and_model_typed() {
    let model = AgentModel::new("spike-model").with_provider("spike");
    assert_eq!(model.provider(), Some("spike"));
    assert_eq!(model.model(), "spike-model");

    let invocation = AgentRuntimeInvocation::new("inspect the workspace")
        .with_model(model)
        .with_session_directory("/tmp/pi-sessions");
    let command = AgentRuntime::Pi
        .command(&invocation, AgentRuntimeCapabilities::default())
        .expect("valid Pi model selection should build a command");
    let args = command_args(&command);
    assert!(args.windows(2).any(|pair| pair == ["--provider", "spike"]));
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--model", "spike-model"])
    );
}

#[test]
fn pi_requires_a_provider_and_model_before_launch() {
    let missing_provider =
        AgentRuntimeInvocation::new("work").with_model(AgentModel::new("spike-model"));
    let error = AgentRuntime::Pi
        .command(&missing_provider, AgentRuntimeCapabilities::default())
        .expect_err("Pi must reject a model without a provider");
    assert!(error.to_string().contains("provider"));

    let missing_model =
        AgentRuntimeInvocation::new("work").with_model(AgentModel::new(" ").with_provider("spike"));
    let error = AgentRuntime::Pi
        .command(&missing_model, AgentRuntimeCapabilities::default())
        .expect_err("Pi must reject a blank model");
    assert!(error.to_string().contains("model"));
}

#[test]
fn fresh_pi_command_uses_explicit_json_model_and_trusted_workspace_policy() {
    let prompt = "inspect; echo $(touch /tmp/not-a-command)";
    let system_prompt = "follow the task rules";
    let invocation = AgentRuntimeInvocation::new(prompt)
        .with_model(AgentModel::new("spike-model").with_provider("spike"))
        .with_name("worker session")
        .with_system_prompt(system_prompt)
        .with_session_directory("/tmp/pi sessions");
    let command = AgentRuntime::Pi
        .command(&invocation, AgentRuntimeCapabilities::default())
        .expect("valid Pi invocation should build");
    let args = command_args(&command);

    assert_eq!(command.get_program(), "pi");
    assert_eq!(
        &args[..16],
        [
            "--mode",
            "json",
            "--provider",
            "spike",
            "--model",
            "spike-model",
            "--session-dir",
            "/tmp/pi sessions",
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-themes",
            "--no-context-files",
            "--no-approve",
            "--tools",
            "read,bash,edit,write,grep,find,ls",
        ]
    );
    assert_eq!(args[16], "--name");
    assert_eq!(args[17], "worker session");
    assert_eq!(args[18], "--system-prompt");
    assert!(args[19].starts_with("follow the task rules\n\nDelegated work is read-only."));
    assert_eq!(args[20], prompt);
    assert!(!args.iter().any(|arg| arg == "sh" || arg == "-c"));
}

#[test]
fn resumed_pi_command_selects_the_existing_session_without_repeating_system_prompt() {
    let invocation = AgentRuntimeInvocation::new("continue; echo '$HOME'")
        .with_model(AgentModel::new("spike-model").with_provider("spike"))
        .with_session_id("session id with 'quotes'")
        .with_system_prompt("must not be repeated")
        .with_session_directory("/tmp/pi sessions");
    let command = AgentRuntime::Pi
        .command(&invocation, AgentRuntimeCapabilities::default())
        .expect("valid resumed Pi invocation should build");
    let args = command_args(&command);

    assert!(
        args.windows(2)
            .any(|pair| pair == ["--session", "session id with 'quotes'"])
    );
    assert!(!args.iter().any(|arg| arg == "--system-prompt"));
    assert!(
        args.last()
            .expect("prompt argument")
            .ends_with("\n\ncontinue; echo '$HOME'")
    );
}

#[test]
fn fresh_claude_command_preserves_headless_stream_invocation() {
    let invocation = AgentRuntimeInvocation::new("implement the thing")
        .with_model("opus")
        .with_name("pool:worker-abc:AMBA-42")
        .with_system_prompt("dispatch rules");
    let command = AgentRuntime::Claude
        .command(&invocation, AgentRuntimeCapabilities::new(false, true))
        .expect("Claude command should build");

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
    let command = AgentRuntime::Claude
        .command(&invocation, AgentRuntimeCapabilities::default())
        .expect("Claude command should build");

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
    let command = AgentRuntime::Codex
        .command(&invocation, AgentRuntimeCapabilities::new(true, false))
        .expect("Codex command should build");

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
    let command = AgentRuntime::Codex
        .command(&invocation, AgentRuntimeCapabilities::new(true, false))
        .expect("Codex command should build");

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
    let claude = AgentRuntime::Claude
        .command(
            &AgentRuntimeInvocation::new("work"),
            AgentRuntimeCapabilities::new(true, false),
        )
        .expect("Claude command should build");
    let codex = AgentRuntime::Codex
        .command(
            &AgentRuntimeInvocation::new("work"),
            AgentRuntimeCapabilities::new(false, true),
        )
        .expect("Codex command should build");

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
fn pi_probe_accepts_the_observed_version_and_required_flags() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    write_executable(
        &temporary_directory.path().join("pi"),
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then printf '%s\n' '0.84.1'; exit 0; fi
printf '%s\n' '--mode --provider --model --session --session-dir --system-prompt --name --tools --no-extensions --no-skills --no-prompt-templates --no-themes --no-context-files --no-approve'
"#,
    );
    let result = temporary_directory.path().join("result");
    let output = helper_command(temporary_directory.path(), &result, "pi")
        .output()
        .expect("Pi probe helper should run");

    assert!(
        output.status.success(),
        "Pi probe helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(result).expect("probe helper result"),
        "ok"
    );
}

#[test]
fn pi_probe_reports_failed_version_checks() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    write_executable(
        &temporary_directory.path().join("pi"),
        "#!/bin/sh
printf '%s\\n' 'provider unavailable' >&2
exit 7
",
    );
    let result = temporary_directory.path().join("result");
    let output = helper_command(temporary_directory.path(), &result, "pi-failed")
        .output()
        .expect("Pi probe helper should run");

    assert!(
        output.status.success(),
        "Pi probe helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let error = fs::read_to_string(result).expect("probe helper result");
    assert!(error.contains("pi version capability probe failed"));
    assert!(error.contains("status 7"));
}

#[test]
fn pi_probe_rejects_malformed_version_output() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    write_executable(
        &temporary_directory.path().join("pi"),
        "#!/bin/sh
printf '%s\\n' 'not-a-version'
",
    );
    let result = temporary_directory.path().join("result");
    let output = helper_command(temporary_directory.path(), &result, "pi-malformed")
        .output()
        .expect("Pi probe helper should run");

    assert!(output.status.success());
    let error = fs::read_to_string(result).expect("probe helper result");
    assert!(error.contains("pi capability probe returned malformed data"));
}

#[test]
fn pi_probe_rejects_a_missing_required_flag() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    write_executable(
        &temporary_directory.path().join("pi"),
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then printf '%s\n' '0.84.1'; exit 0; fi
printf '%s\n' '--mode --provider --model'
"#,
    );
    let result = temporary_directory.path().join("result");
    let output = helper_command(temporary_directory.path(), &result, "pi-unsupported")
        .output()
        .expect("Pi probe helper should run");

    assert!(output.status.success());
    let error = fs::read_to_string(result).expect("probe helper result");
    assert!(error.contains("pi does not support required capability"));
}

#[test]
fn missing_pi_executable_identifies_pi_in_probe_error() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let result = temporary_directory.path().join("result");
    let output = helper_command(temporary_directory.path(), &result, "pi-missing")
        .output()
        .expect("probe helper should run");

    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(result).expect("probe helper result"),
        "pi executable not found in PATH"
    );
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
        .args([
            "--config",
            "signing.behavior=drop",
            "git",
            "init",
            "--no-colocate",
        ])
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

/// A reserved background Run must execute in its own Worker Workspace, never in
/// the directory jjfx was invoked from. Every Worker in one dispatch sharing the
/// caller's cwd would make their changes inseparable.
#[test]
fn reserved_background_run_uses_the_worker_workspace_as_cwd() {
    let temporary_directory = TempDir::new().expect("temporary directory");
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
    let cwd_result = temporary_directory.path().join("cwd");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    write_executable(
        &bin_directory.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\npwd > \"$WSG_AGENT_RUNTIME_HELPER_WORKSPACE\"\n",
    );
    let path = env::join_paths([
        bin_directory.as_os_str(),
        PathBuf::from("/usr/bin").as_os_str(),
        PathBuf::from("/bin").as_os_str(),
    ])
    .expect("runtime PATH");
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args(["--exact", "reserved_background_run_cwd_helper", "--ignored"])
        .env("PATH", path)
        .env(HELPER_REPOSITORY, temporary_directory.path())
        .env(HELPER_WORKER, worker_id.as_str())
        .env(HELPER_WORKSPACE, &cwd_result)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let output = helper.output().expect("helper should run");
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let expected = repository
        .root()
        .parent()
        .expect("temporary repository should have a parent")
        .join(format!(
            "{}-workspaces/{}",
            repository
                .root()
                .file_name()
                .expect("temporary repository should have a name")
                .to_string_lossy(),
            worker_id.as_str()
        ))
        .canonicalize()
        .expect("canonical Worker Workspace");
    let actual = fs::read_to_string(&cwd_result)
        .expect("runtime should record its cwd")
        .trim()
        .to_owned();
    assert_eq!(
        actual,
        expected.display().to_string(),
        "a background Run must execute in its own Worker Workspace"
    );
}

#[test]
fn reserved_background_run_releases_worker_when_runtime_probe_fails() {
    let temporary_directory = TempDir::new().expect("temporary directory");
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
        .args([
            "--config",
            "signing.behavior=drop",
            "git",
            "init",
            "--no-colocate",
        ])
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
        jjfx::engine::Loaded::Present(versioned) => versioned,
        jjfx::engine::Loaded::Missing => panic!("reserved Worker state should exist"),
    };
    let outcome = worker_state
        .commit(
            Expected::Match(loaded.revision().clone()),
            StateChange::Remove,
        )
        .expect("remove Worker state for the race");
    assert!(matches!(outcome, jjfx::engine::CommitOutcome::Applied(_)));
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
        .args([
            "--config",
            "signing.behavior=drop",
            "git",
            "init",
            "--no-colocate",
        ])
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
        jjfx::engine::Loaded::Present(versioned) => versioned,
        jjfx::engine::Loaded::Missing => panic!("reserved Worker state should exist"),
    };
    let outcome = worker_state
        .commit(
            Expected::Match(loaded.revision().clone()),
            StateChange::Remove,
        )
        .expect("remove Worker state for the race");
    assert!(matches!(outcome, jjfx::engine::CommitOutcome::Applied(_)));
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
    let deadline = Instant::now() + HELPER_LIVENESS_TIMEOUT;
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
    let deadline = Instant::now() + HELPER_LIVENESS_TIMEOUT;
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
fn foreground_run_reads_the_terminal_and_returns_it() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let bin_directory = temporary_directory.path().join("bin");
    let workspace = temporary_directory.path().join("workspace");
    let log = temporary_directory.path().join("worker.log");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    fs::create_dir(&workspace).expect("Worker Workspace");
    // The Agent reads the terminal. While the Run sat in a background process
    // group that read stopped on SIGTTIN, so the Run never finished. It exits
    // non-zero so the wrapper's exit-code passthrough is covered too.
    write_executable(
        &bin_directory.join("claude"),
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf 'agent-ready\\n'\nread line\nprintf 'agent-read:%s\\n' \"$line\"\nexit 7\n",
    );

    let pty = open_pty();
    let slave = open_pty_slave(&pty.slave_path);
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args([
            "--exact",
            "foreground_terminal_helper",
            "--ignored",
            "--nocapture",
        ])
        .env("PATH", runtime_path(&bin_directory))
        .env(HELPER_WORKSPACE, &workspace)
        .env(HELPER_LOG, &log)
        .env(HELPER_PTY_SLAVE, &pty.slave_path)
        // The supervisor re-invokes its own executable as the wrapper; this test
        // binary is not that executable, so point it at the real one.
        .env(
            jjfx::engine::FOREGROUND_WRAPPER_ENV,
            env!("CARGO_BIN_EXE_jjfx"),
        )
        .stdin(Stdio::from(slave.try_clone().expect("PTY slave stdin")))
        .stdout(Stdio::from(slave.try_clone().expect("PTY slave stdout")))
        .stderr(Stdio::from(slave));
    let mut child = helper.spawn().expect("foreground helper");
    let mut terminal = PtyOutput::drain(&pty.master);

    // The Run owns its own process group, yet the Agent still reads the terminal.
    terminal.wait_for("agent-ready");
    rustix::io::write(&pty.master, b"input-value\n").expect("write to the terminal");
    terminal.wait_for("agent-read:input-value");

    // The supervisor is the terminal's foreground group again once the Run ends,
    // and can read the terminal itself.
    let report = terminal.wait_for("after-run");
    let report = report
        .lines()
        .find(|line| line.contains("after-run"))
        .expect("after-run report");
    assert!(
        report.contains("restored=true") && report.contains("exit=Some(7)"),
        "the Run should hand the terminal back and report the Agent's exit: {report}"
    );
    rustix::io::write(&pty.master, b"input-after\n").expect("write after the Run");
    terminal.wait_for("after-read:input-after");

    let status = child.wait().expect("foreground helper");
    assert!(
        status.success(),
        "foreground helper failed: {}",
        terminal.buffer()
    );
}

#[test]
#[ignore]
fn foreground_terminal_helper() {
    // Become a session leader and acquire the PTY as the controlling terminal, so
    // the Run's handover has a terminal to hand over.
    rustix::process::setsid().expect("session leader");
    let terminal = rustix::fs::open(
        PathBuf::from(env::var_os(HELPER_PTY_SLAVE).expect("PTY slave path")),
        rustix::fs::OFlags::RDWR,
        rustix::fs::Mode::empty(),
    )
    .expect("controlling terminal");
    rustix::stdio::dup2_stdin(&terminal).expect("PTY stdin");
    rustix::stdio::dup2_stdout(&terminal).expect("PTY stdout");
    rustix::stdio::dup2_stderr(&terminal).expect("PTY stderr");

    let workspace = PathBuf::from(env::var_os(HELPER_WORKSPACE).expect("Worker Workspace"));
    let log = PathBuf::from(env::var_os(HELPER_LOG).expect("log path"));
    let request = RunRequest::new(
        AgentRuntime::Claude,
        AgentRuntimeInvocation::new("foreground terminal test"),
        workspace,
        log,
    );
    let outcome = RunSupervisor::new()
        .run_foreground(&request)
        .expect("foreground Run should complete");
    let restored =
        rustix::termios::tcgetpgrp(std::io::stdin()).ok() == Some(rustix::process::getpgrp());
    println!(
        "after-run exit={:?} restored={restored}",
        outcome.exit_code()
    );

    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .expect("read the terminal after the Run");
    println!("after-read:{}", line.trim_end());
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
        .args([
            "--config",
            "signing.behavior=drop",
            "git",
            "init",
            "--no-colocate",
        ])
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
fn reserved_background_pi_run_uses_shared_pid_and_finalization_supervision() {
    let temporary_directory = TempDir::new().expect("temporary directory");
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
    let pool_path = temporary_directory.path().join(".jj/pool.json");
    let mut pool: Value =
        serde_json::from_slice(&fs::read(&pool_path).expect("pool state")).expect("pool JSON");
    pool["agent"] = "pi".into();
    fs::write(
        &pool_path,
        serde_json::to_vec(&pool).expect("pool JSON serialization"),
    )
    .expect("configured Pi pool state");
    let bin_directory = temporary_directory.path().join("bin");
    let result = temporary_directory.path().join("result");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    write_executable(
        &bin_directory.join("pi"),
        concat!(
            "#!/bin/sh\n",
            "if [ \"$1\" = \"--version\" ]; then printf '%s\\n' '0.84.1'; exit 0; fi\n",
            "if [ \"$1\" = \"--help\" ]; then printf '%s\\n' '--mode --provider --model --session --session-dir --system-prompt --name --tools --no-extensions --no-skills --no-prompt-templates --no-themes --no-context-files --no-approve'; exit 0; fi\n",
            "printf '%s\\n' '{\"type\":\"session\",\"version\":3,\"id\":\"pi-worker-session\",\"cwd\":\"/tmp\"}'\n",
            "printf '%s\\n' '{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"Pi completed the reserved Run.\"}],\"stopReason\":\"stop\",\"usage\":{\"input\":3,\"output\":2,\"cacheRead\":0,\"cacheWrite\":0,\"cost\":{\"total\":0.001}}}}'\n",
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
        .env(HELPER_RUNTIME, "pi")
        .env(HELPER_REPOSITORY, temporary_directory.path())
        .env(HELPER_WORKER, worker_id.as_str())
        .env(HELPER_RESULT, &result)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let output = helper
        .output()
        .expect("Pi reserved finalization helper should run");
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(result).expect("reserved finalization result"),
        "exit=Some(0); conclusion=Succeeded; cost=Some(1000); source=Provider"
    );

    let snapshot = repository.worker_pool().snapshot();
    let worker = snapshot
        .worker(worker_id.as_str())
        .expect("finalized Worker");
    assert_eq!(worker.status(), WorkerStatus::Done);
    assert_eq!(worker.exit_code(), Some(0));
    assert!(worker.pid().is_some());
    assert!(worker.completed_at().is_some());
}

/// A fake `opencode` that answers the session API and records what the Run drove.
///
/// `session.create` records the request body and reports `ses_test` anchored to
/// the Worker Workspace; `session.get` reports it anchored to the directory in
/// `WSG_AGENT_RUNTIME_HELPER_OPENCODE_GET_DIRECTORY`; `run` records its cwd and
/// arguments.
const OPENCODE_SESSION_SCRIPT: &str = r#"#!/bin/sh
case "$1" in
  --version) printf '%s\n' '2.0.29'; exit 0 ;;
  api)
    case "$2" in
      session.create)
        printf '%s' "$4" > "__CREATE_BODY__"
        printf '{"data":{"id":"ses_test","location":{"directory":"%s"}}}' "$WSG_AGENT_RUNTIME_HELPER_OPENCODE_WORKSPACE"
        exit 0 ;;
      session.get)
        printf '{"data":{"id":"ses_test","location":{"directory":"%s"}}}' "$WSG_AGENT_RUNTIME_HELPER_OPENCODE_GET_DIRECTORY"
        exit 0 ;;
    esac
    exit 1 ;;
  run)
    { pwd; printf '%s\n' "$@"; } > "__RUN_ARGS__"
    exit 0 ;;
esac
exit 1
"#;

/// A fake `opencode` whose API is unreachable, so the Run must fall back.
const OPENCODE_UNAVAILABLE_SCRIPT: &str = r#"#!/bin/sh
case "$1" in
  --version) printf '%s\n' '2.0.29'; exit 0 ;;
  api) printf '%s\n' 'Error: Could not reach server' >&2; exit 1 ;;
  run)
    { pwd; printf '%s\n' "$@"; } > "__RUN_ARGS__"
    exit 0 ;;
esac
exit 1
"#;

/// The repository, Worker, and paths one opencode Run session test needs.
struct OpencodeSessionFixture {
    _temporary_directory: TempDir,
    repository: Repository,
    worker_id: WorkerId,
    workspace: PathBuf,
    create_body: PathBuf,
    run_args: PathBuf,
    bin_directory: PathBuf,
    result: PathBuf,
}

fn opencode_session_fixture() -> OpencodeSessionFixture {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let repository = initialize_repository(temporary_directory.path());
    let growth = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"))
        .expect("Worker Workspace should be provisioned");
    let worker_id = growth.added_workers()[0].clone();
    // Configure the Pool for opencode so the Reservation selects it.
    let pool_path = temporary_directory.path().join(".jj/pool.json");
    let mut pool: Value =
        serde_json::from_slice(&fs::read(&pool_path).expect("pool state")).expect("pool JSON");
    pool["agent"] = "opencode".into();
    fs::write(&pool_path, serde_json::to_vec(&pool).expect("pool JSON")).expect("configured pool");
    // The Worker Workspace is a sibling of the repository root, named after it.
    let root = repository.root();
    let workspace = root
        .parent()
        .expect("workspace base parent")
        .join(format!(
            "{}-workspaces",
            root.file_name().expect("repository name").to_string_lossy()
        ))
        .join(worker_id.as_str());
    let bin_directory = temporary_directory.path().join("bin");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    OpencodeSessionFixture {
        create_body: temporary_directory.path().join("opencode-create-body"),
        run_args: temporary_directory.path().join("opencode-run-args"),
        result: temporary_directory.path().join("result"),
        _temporary_directory: temporary_directory,
        repository,
        worker_id,
        workspace,
        bin_directory,
    }
}

impl OpencodeSessionFixture {
    fn write_opencode(&self, script: &str) {
        write_executable(
            &self.bin_directory.join("opencode"),
            &script
                .replace("__CREATE_BODY__", &self.create_body.display().to_string())
                .replace("__RUN_ARGS__", &self.run_args.display().to_string()),
        );
    }

    fn spawn_helper(&self, get_directory: &Path, session_id: Option<&str>) -> std::process::Output {
        let mut helper = Command::new(env::current_exe().expect("test executable"));
        helper
            .args(["--exact", "opencode_run_session_helper", "--ignored"])
            .env("PATH", runtime_path(&self.bin_directory))
            .env(HELPER_OPENCODE_WORKSPACE, &self.workspace)
            .env(HELPER_OPENCODE_GET_DIRECTORY, get_directory)
            .env(HELPER_REPOSITORY, self.repository.root())
            .env(HELPER_WORKER, self.worker_id.as_str())
            .env(HELPER_RESULT, &self.result)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(session_id) = session_id {
            helper.env(HELPER_OPENCODE_SESSION_ID, session_id);
        }
        helper.output().expect("opencode session helper should run")
    }

    fn recorded_session(&self) -> Option<String> {
        self.repository
            .state_store()
            .sessions()
            .session(&self.worker_id)
            .expect("read recorded session")
    }

    /// Records a session as a previous Run would have, so a later Run has a
    /// stale one to forget.
    fn record_session(&self, session_id: &str) {
        self.repository
            .state_store()
            .sessions()
            .record(&self.worker_id, session_id)
            .expect("record session");
    }

    /// The cwd the fake `opencode run` saw, and the arguments it was given.
    fn recorded_run(&self) -> (PathBuf, Vec<String>) {
        let contents = fs::read_to_string(&self.run_args).expect("recorded opencode run");
        let mut lines = contents.lines();
        let cwd = PathBuf::from(lines.next().expect("recorded cwd"));
        (cwd, lines.map(str::to_owned).collect())
    }
}

#[test]
fn opencode_run_creates_a_session_anchored_to_the_workspace_and_drives_it() {
    let fixture = opencode_session_fixture();
    fixture.write_opencode(OPENCODE_SESSION_SCRIPT);

    let output = fixture.spawn_helper(&fixture.workspace, None);
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(&fixture.result).expect("helper result"),
        "ok exit=Some(0)"
    );

    // The session was created with the Worker Workspace as its location.
    let body: Value = serde_json::from_slice(
        &fs::read(&fixture.create_body).expect("recorded session.create body"),
    )
    .expect("session.create body JSON");
    assert_eq!(
        body["location"]["directory"].as_str(),
        Some(fixture.workspace.to_str().expect("workspace UTF-8"))
    );
    let expected_title = format!("pool:{}:ENG-42", fixture.worker_id);
    assert_eq!(body["title"].as_str(), Some(expected_title.as_str()));

    // The Run was driven with the created session, in the Worker Workspace.
    let (cwd, arguments) = fixture.recorded_run();
    assert_eq!(
        cwd.canonicalize().expect("run cwd canonical"),
        fixture
            .workspace
            .canonicalize()
            .expect("workspace canonical")
    );
    assert!(
        arguments
            .windows(2)
            .any(|pair| pair == ["--session", "ses_test"]),
        "the Run should drive the created session: {arguments:?}"
    );

    // The id survives for a later process to identify the Run's session.
    assert_eq!(fixture.recorded_session().as_deref(), Some("ses_test"));
}

#[test]
fn opencode_run_falls_back_when_the_session_api_is_unavailable() {
    let fixture = opencode_session_fixture();
    fixture.write_opencode(OPENCODE_UNAVAILABLE_SCRIPT);

    let output = fixture.spawn_helper(&fixture.workspace, None);
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(&fixture.result).expect("helper result"),
        "ok exit=Some(0)"
    );

    // Today's invocation still runs, in the Worker Workspace, without a session.
    let (cwd, arguments) = fixture.recorded_run();
    assert_eq!(
        cwd.canonicalize().expect("run cwd canonical"),
        fixture
            .workspace
            .canonicalize()
            .expect("workspace canonical")
    );
    assert!(
        !arguments.iter().any(|argument| argument == "--session"),
        "an unavailable API must not drive a session: {arguments:?}"
    );
    assert_eq!(fixture.recorded_session(), None);
}

/// A Run that drives no session must forget the one a previous Run recorded.
/// Leaving it in place would let a later reconcile read that Run's outcome and
/// finish this Worker while its work is still going.
#[test]
fn opencode_run_without_a_session_forgets_a_previously_recorded_one() {
    let fixture = opencode_session_fixture();
    fixture.write_opencode(OPENCODE_UNAVAILABLE_SCRIPT);
    fixture.record_session("ses_stale");

    let output = fixture.spawn_helper(&fixture.workspace, None);
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        fixture.recorded_session(),
        None,
        "a Run that drives no session must not leave a previous Run's session recorded"
    );
}

#[test]
fn opencode_run_resumes_and_records_an_existing_session() {
    let fixture = opencode_session_fixture();
    fixture.write_opencode(OPENCODE_SESSION_SCRIPT);

    // A resumed Run already carries a session id: it must drive that session and
    // record it, without creating another.
    let output = fixture.spawn_helper(&fixture.workspace, Some("ses_existing"));
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(&fixture.result).expect("helper result"),
        "ok exit=Some(0)"
    );

    let (_, arguments) = fixture.recorded_run();
    assert!(
        arguments
            .windows(2)
            .any(|pair| pair == ["--session", "ses_existing"]),
        "the Run should drive the resumed session: {arguments:?}"
    );
    assert_eq!(fixture.recorded_session().as_deref(), Some("ses_existing"));
    assert!(
        !fixture.create_body.exists(),
        "a resume must not create another session"
    );
}

#[test]
fn opencode_run_fails_when_the_session_is_anchored_elsewhere() {
    let fixture = opencode_session_fixture();
    fixture.write_opencode(OPENCODE_SESSION_SCRIPT);

    // opencode reports the session anchored to the repository, not the Workspace.
    let elsewhere = fixture.repository.root().to_owned();
    let output = fixture.spawn_helper(&elsewhere, None);
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = fs::read_to_string(&fixture.result).expect("helper result");
    assert!(
        result.contains("instead of the Worker Workspace"),
        "the mismatch should fail the Run: {result}"
    );

    // The Run never spawned, nothing was recorded, and the Worker was released.
    assert!(!fixture.run_args.exists(), "the Run must not start");
    assert_eq!(fixture.recorded_session(), None);
    let snapshot = fixture.repository.worker_pool().snapshot();
    let worker = snapshot
        .worker(fixture.worker_id.as_str())
        .expect("released Worker");
    assert_eq!(worker.status(), WorkerStatus::Idle);
}

#[test]
#[ignore]
fn opencode_run_session_helper() {
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
        .reserve_named(worker_id.clone(), "ENG-42")
        .expect("Worker reservation");
    let mut invocation =
        AgentRuntimeInvocation::new("do the work").with_name(format!("pool:{worker_id}:ENG-42"));
    if let Some(session_id) = env::var_os(HELPER_OPENCODE_SESSION_ID) {
        invocation = invocation.with_session_id(session_id.to_string_lossy().into_owned());
    }
    let result = env::var_os(HELPER_RESULT).expect("result path");
    match RunSupervisor::new().run_reserved_background(reservation, invocation) {
        Ok(background) => {
            let outcome = background.wait().expect("background Run should complete");
            fs::write(result, format!("ok exit={:?}", outcome.exit_code())).expect("result");
        }
        Err(error) => {
            fs::write(result, format!("err {error}")).expect("result");
        }
    }
}

/// A fake `opencode` that serves the session API for the stop and liveness
/// paths. `session.interrupt` records the session it was asked to stop;
/// `session.get` reports the outcome named by `..._OPENCODE_OUTCOME` (none by
/// default, so the session looks open); `session.active`/`session.list` serve
/// the title guard. Any API call fails when `..._OPENCODE_UNAVAILABLE` is set.
const OPENCODE_CONTROL_SCRIPT: &str = r#"#!/bin/sh
case "$1" in
  --version) printf '%s\n' '2.0.29'; exit 0 ;;
  api)
    if [ -n "$WSG_AGENT_RUNTIME_HELPER_OPENCODE_UNAVAILABLE" ]; then
      printf '%s\n' 'Error: Could not reach server' >&2
      exit 1
    fi
    case "$2" in
      session.get)
        if [ -n "$WSG_AGENT_RUNTIME_HELPER_OPENCODE_OUTCOME" ]; then
          printf '{"data":{"id":"ses_test","outcome":"%s"}}' "$WSG_AGENT_RUNTIME_HELPER_OPENCODE_OUTCOME"
        else
          printf '{"data":{"id":"ses_test"}}'
        fi
        exit 0 ;;
      session.active)
        printf '{"data":%s}' "${WSG_AGENT_RUNTIME_HELPER_OPENCODE_ACTIVE:-{}}"
        exit 0 ;;
      session.list)
        printf '{"data":%s}' "${WSG_AGENT_RUNTIME_HELPER_OPENCODE_LIST:-[]}"
        exit 0 ;;
      session.interrupt)
        printf '%s' "$4" > "__INTERRUPT__"
        printf '{"interrupted":true}'
        exit 0 ;;
    esac
    exit 1 ;;
esac
exit 1
"#;

/// The repository, Worker, and paths the stop and liveness tests need.
struct OpencodeControlFixture {
    _temporary_directory: TempDir,
    repository: Repository,
    worker_id: WorkerId,
    bin_directory: PathBuf,
    result: PathBuf,
    interrupt: PathBuf,
}

fn opencode_control_fixture() -> OpencodeControlFixture {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let repository = initialize_repository(temporary_directory.path());
    let worker_id = grow_one_worker(&repository);
    let bin_directory = temporary_directory.path().join("bin");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    OpencodeControlFixture {
        result: temporary_directory.path().join("result"),
        interrupt: temporary_directory.path().join("interrupt"),
        _temporary_directory: temporary_directory,
        repository,
        worker_id,
        bin_directory,
    }
}

impl OpencodeControlFixture {
    fn write_opencode(&self) {
        write_executable(
            &self.bin_directory.join("opencode"),
            &OPENCODE_CONTROL_SCRIPT
                .replace("__INTERRUPT__", &self.interrupt.display().to_string()),
        );
    }

    /// Writes a busy Run for this Worker, so a stop or a reconciliation has
    /// something to act on.
    fn write_busy_worker(&self, agent: &str, pid: u32, ticket: &str) {
        self.write_busy_worker_with_log(agent, pid, ticket, "/repo/.jj/pool/worker.log");
    }

    /// The same, with the Run log pointed somewhere a test can populate.
    fn write_busy_worker_with_log(&self, agent: &str, pid: u32, ticket: &str, log_file: &str) {
        write_worker_json(
            &worker_state_path(self.repository.root(), &self.worker_id),
            serde_json::json!({
                "status": "busy",
                "agent": agent,
                "ticket": ticket,
                "pid": pid,
                "started_at": "2026-07-30T10:00:00Z",
                "completed_at": Value::Null,
                "log_file": log_file,
                "branch_name": ticket.to_lowercase(),
                "exit_code": Value::Null,
                "error": Value::Null
            }),
        );
    }

    /// Writes a Run log beside the Worker state and returns its path.
    fn write_run_log(&self, contents: &str) -> String {
        let path = self.repository.root().join("worker.log");
        fs::write(&path, contents).expect("run log");
        path.display().to_string()
    }

    /// Records the session the Worker's Run drives, as ticket 38 would.
    fn record_session(&self, session_id: &str) {
        self.repository
            .state_store()
            .sessions()
            .record(&self.worker_id, session_id)
            .expect("record session");
    }

    fn run_helper(&self, action: &str) -> std::process::Output {
        self.run_helper_with(action, &[])
    }

    fn run_helper_with(&self, action: &str, extra: &[(&str, &str)]) -> std::process::Output {
        let mut helper = Command::new(env::current_exe().expect("test executable"));
        helper
            .args(["--exact", "opencode_control_helper", "--ignored"])
            .env("PATH", runtime_path(&self.bin_directory))
            .env(HELPER_OPENCODE_ACTION, action)
            .env(HELPER_REPOSITORY, self.repository.root())
            .env(HELPER_WORKER, self.worker_id.as_str())
            .env(HELPER_RESULT, &self.result)
            .env(HELPER_OPENCODE_INTERRUPT, &self.interrupt)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        for (key, value) in extra {
            helper.env(key, value);
        }
        helper.output().expect("opencode control helper should run")
    }
}

/// Stops a Worker or reconciles its Run in a process whose `PATH` carries the
/// fake `opencode`, so the API seam is exercised without a live server.
#[test]
#[ignore]
fn opencode_control_helper() {
    let repository = Repository::open(
        env::var_os(HELPER_REPOSITORY)
            .expect("repository")
            .as_os_str(),
    )
    .expect("repository");
    let worker_id =
        WorkerId::parse(env::var(HELPER_WORKER).expect("Worker ID")).expect("Worker ID");
    let result = env::var_os(HELPER_RESULT).expect("result path");
    let outcome = match env::var(HELPER_OPENCODE_ACTION).as_deref() {
        Ok("reset") => match RunSupervisor::new().reset_run(&repository, &worker_id) {
            Ok(RunReset::Abandoned { terminated_pid }) => format!("abandoned {terminated_pid:?}"),
            Ok(other) => format!("{other:?}"),
            Err(error) => format!("err {error}"),
        },
        Ok("reconcile") => {
            let snapshot = repository.worker_pool().reconcile_runs();
            let worker = snapshot
                .worker(worker_id.as_str())
                .expect("reconciled Worker");
            format!("{:?}", worker.status())
        }
        other => panic!("unknown opencode control action {other:?}"),
    };
    fs::write(result, outcome).expect("result");
}

/// Stopping an opencode Worker interrupts the session recorded for it instead
/// of signalling the client process.
#[test]
fn reset_run_interrupts_an_opencode_run_through_the_daemon() {
    let fixture = opencode_control_fixture();
    fixture.write_opencode();
    let mut leader = spawn_group_leader();
    let pid = leader.id();
    fixture.write_busy_worker("opencode", pid, "ENG-42");
    fixture.record_session("ses_run");

    let output = fixture.run_helper("reset");
    let reset = fs::read_to_string(&fixture.result).expect("helper result");
    let leader_alive = test_kill_process(unix_pid(pid)).is_ok();
    if leader_alive {
        let _ = kill_process(unix_pid(pid), Signal::KILL);
    }
    let _ = leader.wait();

    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The stop went to the daemon session, not the client's process group.
    assert_eq!(reset, "abandoned None");
    assert_eq!(
        fs::read_to_string(&fixture.interrupt).expect("recorded interrupt"),
        "sessionID=ses_run"
    );
    assert!(
        leader_alive,
        "a daemon interrupt is the stop; the client process must not be signalled"
    );
    let snapshot = fixture.repository.worker_pool().snapshot();
    let worker = snapshot
        .worker(fixture.worker_id.as_str())
        .expect("reset Worker");
    assert_eq!(worker.status(), WorkerStatus::Idle);
}

/// When the session API is unreachable, stopping an opencode Worker falls back
/// to the process-group signal that still owns the client.
#[test]
fn reset_run_falls_back_to_the_process_group_when_the_session_api_is_unavailable() {
    let fixture = opencode_control_fixture();
    fixture.write_opencode();
    let mut leader = spawn_group_leader();
    let pid = leader.id();
    // Reap the leader as soon as the signal kills it: a zombie keeps its
    // process group alive, and the fallback signal waits for the group to go.
    let reaper = thread::spawn(move || {
        let _ = leader.wait();
    });
    fixture.write_busy_worker("opencode", pid, "ENG-42");
    fixture.record_session("ses_run");

    let output = fixture.run_helper_with("reset", &[(HELPER_OPENCODE_UNAVAILABLE, "1")]);
    let reset = fs::read_to_string(&fixture.result).expect("helper result");
    let group_gone = test_kill_process_group(unix_pid(pid)).is_err();
    if !group_gone {
        let _ = kill_process_group(unix_pid(pid), Signal::KILL);
    }
    let _ = reaper.join();

    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(reset, format!("abandoned Some({pid})"));
    assert!(
        group_gone,
        "an unavailable API must fall back to the Run's process group"
    );
    assert!(
        !fixture.interrupt.exists(),
        "an unavailable API must not have interrupted a session"
    );
}

/// A Worker whose opencode session is still open is not finished, even though
/// its recorded client pid is already gone.
#[test]
fn reconcile_runs_keeps_a_busy_opencode_worker_busy_while_its_session_runs() {
    let fixture = opencode_control_fixture();
    fixture.write_opencode();
    fixture.write_busy_worker("opencode", dead_pid(), "ENG-42");
    fixture.record_session("ses_run");

    let output = fixture.run_helper("reconcile");
    let status = fs::read_to_string(&fixture.result).expect("helper result");

    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        status, "Busy",
        "a Run whose session is still open must stay busy"
    );
}

/// A Worker is finished on its session's outcome even while its client pid is
/// still alive: the pid is only a daemon client.
#[test]
fn reconcile_runs_finishes_a_busy_opencode_worker_on_its_session_outcome() {
    let fixture = opencode_control_fixture();
    fixture.write_opencode();
    let mut leader = spawn_group_leader();
    let pid = leader.id();
    fixture.write_busy_worker("opencode", pid, "ENG-42");
    fixture.record_session("ses_run");

    let output = fixture.run_helper_with("reconcile", &[(HELPER_OPENCODE_OUTCOME, "succeeded")]);
    let status = fs::read_to_string(&fixture.result).expect("helper result");
    let leader_alive = test_kill_process(unix_pid(pid)).is_ok();
    if leader_alive {
        let _ = kill_process(unix_pid(pid), Signal::KILL);
    }
    let _ = leader.wait();

    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        status, "Done",
        "a session with an outcome must finish the Run"
    );
}

/// Liveness is `session outcome` *or* a Run-log result: a Run whose session is
/// still open but whose log carries a provider terminal event is finished. An
/// open session is not evidence of a running one - opencode reports a session
/// as active while it is merely open - so the log result must not be ignored.
#[test]
fn reconcile_runs_finishes_a_busy_opencode_worker_on_a_run_log_result() {
    let fixture = opencode_control_fixture();
    fixture.write_opencode();
    let log = fixture.write_run_log("{\"type\":\"error\",\"error\":{\"message\":\"boom\"}}\n");
    fixture.write_busy_worker_with_log("opencode", dead_pid(), "ENG-42", &log);
    fixture.record_session("ses_run");

    let output = fixture.run_helper("reconcile");
    let status = fs::read_to_string(&fixture.result).expect("helper result");

    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        status, "Failed",
        "a Run log with a provider terminal event must finish the Run even while its session is open"
    );
}

/// Spawns a live process that leads its own process group, standing in for the
/// client a background Run records.
fn spawn_group_leader() -> Child {
    Command::new("sleep")
        .arg("30")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("group leader helper process")
}

#[test]
fn reserved_background_run_finalizes_failed_worker_after_wait() {
    let temporary_directory = TempDir::new().expect("temporary directory");
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
        .args([
            "--config",
            "signing.behavior=drop",
            "git",
            "init",
            "--no-colocate",
        ])
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
        .args([
            "--config",
            "signing.behavior=drop",
            "git",
            "init",
            "--no-colocate",
        ])
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
        jjfx::engine::Loaded::Present(versioned) => versioned,
        jjfx::engine::Loaded::Missing => panic!("Run A Worker state should exist"),
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
    assert!(matches!(outcome, jjfx::engine::CommitOutcome::Applied(_)));
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
    let invocation = match env::var(HELPER_RUNTIME).as_deref() {
        Ok("pi") => AgentRuntimeInvocation::new("reserved test")
            .with_model(AgentModel::new("test-model").with_provider("test-provider")),
        Ok("claude") | Err(_) => AgentRuntimeInvocation::new("reserved test"),
        Ok(other) => panic!("unknown Agent Runtime {other}"),
    };
    let background = RunSupervisor::new()
        .run_reserved_background(reservation, invocation)
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

/// A foreground Run must own its process group, so a cross-process Reset can
/// terminate it even though it was launched by a different jjfx process.
#[test]
fn cross_process_reset_terminates_a_live_foreground_run() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let repository = initialize_repository(temporary_directory.path());
    let worker_id = grow_one_worker(&repository);

    let bin_directory = temporary_directory.path().join("bin");
    let process = temporary_directory.path().join("process");
    let descendant = temporary_directory.path().join("descendant");
    let diagnostic = temporary_directory.path().join("diagnostic");
    let exit = temporary_directory.path().join("exit");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    write_executable(&bin_directory.join("claude"), STUBBORN_RUNTIME);
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args(["--exact", "reset_run_foreground_helper", "--ignored"])
        .env("PATH", runtime_path(&bin_directory))
        .env(HELPER_REPOSITORY, temporary_directory.path())
        .env(HELPER_WORKER, worker_id.as_str())
        .env(HELPER_PROCESS, &process)
        .env(HELPER_DESCENDANT, &descendant)
        .env(HELPER_DIAGNOSTIC, &diagnostic)
        .env(HELPER_EXIT, &exit)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = helper.spawn().expect("foreground Reset helper");
    wait_for_file(&process);
    wait_for_file(&descendant);
    let leader = read_pid(&process);
    let descendant_pid = read_pid(&descendant);
    wait_for_busy_pid(&repository, &worker_id, leader);

    let reset = RunSupervisor::new()
        .reset_run(&repository, &worker_id)
        .expect("Reset should abandon the foreground Run");
    let group_gone = test_kill_process_group(unix_pid(leader)).is_err();
    let descendant_gone = test_kill_process(unix_pid(descendant_pid)).is_err();
    if !group_gone || !descendant_gone {
        // A surviving Run would otherwise outlive the test; end it directly so
        // the assertions below report the defect instead of leaking processes.
        let _ = kill_process(unix_pid(leader), Signal::KILL);
        let _ = kill_process(unix_pid(descendant_pid), Signal::KILL);
    }
    let output = child.wait_with_output().expect("foreground helper output");

    assert!(
        matches!(reset, RunReset::Abandoned { terminated_pid } if terminated_pid == Some(leader)),
        "a cross-process Reset must report the terminated foreground Run: {reset:?}"
    );
    assert!(
        group_gone,
        "the foreground Run's process group should be gone"
    );
    assert!(
        descendant_gone,
        "a cross-process Reset must terminate the foreground Run's descendants"
    );
    let snapshot = repository.worker_pool().snapshot();
    let worker = snapshot.worker(worker_id.as_str()).expect("reset Worker");
    assert_eq!(worker.status(), WorkerStatus::Idle);
    assert_eq!(worker.pid(), None);
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A foreground launch that fails before its PID is recorded must still remove
/// the whole process group, not just the direct child.
#[test]
fn failed_foreground_launch_leaves_no_surviving_descendants() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let repository = initialize_repository(temporary_directory.path());
    let worker_id = grow_one_worker(&repository);

    let bin_directory = temporary_directory.path().join("bin");
    let result = temporary_directory.path().join("result");
    let process = temporary_directory.path().join("process");
    let descendant = temporary_directory.path().join("descendant");
    let diagnostic = temporary_directory.path().join("diagnostic");
    let reserved = temporary_directory.path().join("reserved");
    let proceed = temporary_directory.path().join("proceed");
    fs::create_dir(&bin_directory).expect("runtime bin directory");
    write_executable(&bin_directory.join("claude"), STUBBORN_RUNTIME);
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args(["--exact", "reserved_foreground_run_helper", "--ignored"])
        .env("PATH", runtime_path(&bin_directory))
        .env(HELPER_REPOSITORY, temporary_directory.path())
        .env(HELPER_WORKER, worker_id.as_str())
        .env(HELPER_RESULT, &result)
        .env(HELPER_PROCESS, &process)
        .env(HELPER_DESCENDANT, &descendant)
        .env(HELPER_DIAGNOSTIC, &diagnostic)
        .env(HELPER_RESERVED, &reserved)
        .env(HELPER_PROCEED, &proceed)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = helper.spawn().expect("foreground helper");

    wait_for_file(&reserved);
    // Replace the reserved Worker state with a FIFO so the PID persistence read
    // blocks until the runtime has spawned its descendant - the launch then
    // fails while the descendant is already running.
    let worker_path = worker_state_path(temporary_directory.path(), &worker_id);
    let state_store = repository.state_store().worker(worker_id.clone());
    let loaded = match state_store.load().expect("reserved Worker state") {
        jjfx::engine::Loaded::Present(versioned) => versioned,
        jjfx::engine::Loaded::Missing => panic!("reserved Worker state should exist"),
    };
    let outcome = state_store
        .commit(
            Expected::Match(loaded.revision().clone()),
            StateChange::Remove,
        )
        .expect("remove Worker state for the race");
    assert!(matches!(outcome, jjfx::engine::CommitOutcome::Applied(_)));
    let fifo_status = Command::new("mkfifo")
        .arg(&worker_path)
        .status()
        .expect("mkfifo should be installed");
    assert!(fifo_status.success(), "mkfifo failed: {fifo_status}");
    fs::write(&proceed, []).expect("allow launch to continue");

    wait_for_file(&process);
    wait_for_file(&descendant);
    let leader = read_pid(&process);
    let descendant_pid = read_pid(&descendant);
    fs::write(&worker_path, b"not-json").expect("release blocked Worker state read");
    fs::remove_file(&worker_path).expect("remove Worker state FIFO");
    fs::write(&worker_path, b"not-json").expect("preserve failed Worker state for release");

    let output = child.wait_with_output().expect("foreground helper output");
    let error = fs::read_to_string(&result).expect("foreground launch error");
    let descendant_alive = test_kill_process(unix_pid(descendant_pid)).is_ok();
    if descendant_alive {
        let _ = kill_process(unix_pid(leader), Signal::KILL);
        let _ = kill_process(unix_pid(descendant_pid), Signal::KILL);
    }

    assert!(error.contains("cannot persist PID"), "{error}");
    assert!(
        output.status.success(),
        "helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !descendant_alive,
        "a failed foreground launch must not leave a surviving descendant"
    );
    assert!(
        test_kill_process_group(unix_pid(leader)).is_err(),
        "the failed foreground Run's process group should be gone"
    );
}

/// A recorded PID that is alive but does not lead a process group is not a Run
/// identity: it must never be signalled, and the unrelated process must survive.
#[test]
fn reset_run_does_not_signal_a_recorded_pid_that_is_not_a_group_leader() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let repository = initialize_repository(temporary_directory.path());
    let worker_id = grow_one_worker(&repository);
    let mut unrelated = spawn_non_group_leader();
    let pid = unrelated.id();
    assert_ne!(
        getpgid(Some(unix_pid(pid))).expect("live process group"),
        unix_pid(pid),
        "the helper process must not lead a process group"
    );
    write_worker_json(
        &worker_state_path(temporary_directory.path(), &worker_id),
        busy_run_state(pid, "ENG-230"),
    );

    let reset = RunSupervisor::new()
        .reset_run(&repository, &worker_id)
        .expect("Reset should abandon the Run");
    let alive = test_kill_process(unix_pid(pid)).is_ok();
    if alive {
        let _ = kill_process(unix_pid(pid), Signal::KILL);
    }
    let _ = unrelated.wait();

    assert!(
        matches!(
            reset,
            RunReset::Abandoned {
                terminated_pid: None
            }
        ),
        "a PID that does not lead a group must not be signalled: {reset:?}"
    );
    assert!(alive, "the unrelated process must never be signalled");
    let snapshot = repository.worker_pool().snapshot();
    let worker = snapshot.worker(worker_id.as_str()).expect("reset Worker");
    assert_eq!(worker.status(), WorkerStatus::Idle);
}

/// The recorded PID is the Run's leader. Once it is gone the recorded group id
/// is a stale hint, so Reset must not signal whatever still answers to it.
#[test]
fn reset_run_does_not_signal_a_group_whose_recorded_leader_is_gone() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let repository = initialize_repository(temporary_directory.path());
    let worker_id = grow_one_worker(&repository);
    let descendant_path = temporary_directory.path().join("orphan-descendant");

    // A leader that spawns a TERM-ignoring descendant and exits, leaving the
    // descendant as the only member of the recorded process group.
    let mut leader = Command::new("sh")
        .arg("-c")
        .arg("( trap '' TERM; while :; do sleep 0.05; done ) & echo $! > \"$1\"; exit 0")
        .arg("sh")
        .arg(&descendant_path)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("group leader helper");
    let leader_pid = leader.id();
    leader.wait().expect("group leader should exit");
    wait_for_file(&descendant_path);
    let descendant_pid = read_pid(&descendant_path);
    assert!(
        test_kill_process_group(unix_pid(leader_pid)).is_ok(),
        "the descendant should keep the recorded process group alive"
    );
    assert!(
        getpgid(Some(unix_pid(leader_pid))).is_err(),
        "the recorded leader should be gone"
    );
    write_worker_json(
        &worker_state_path(temporary_directory.path(), &worker_id),
        busy_run_state(leader_pid, "ENG-231"),
    );

    let reset = RunSupervisor::new()
        .reset_run(&repository, &worker_id)
        .expect("Reset should abandon the Run");
    let descendant_alive = test_kill_process(unix_pid(descendant_pid)).is_ok();
    if descendant_alive {
        let _ = kill_process_group(unix_pid(leader_pid), Signal::KILL);
        let _ = kill_process(unix_pid(descendant_pid), Signal::KILL);
    }

    assert!(
        matches!(
            reset,
            RunReset::Abandoned {
                terminated_pid: None
            }
        ),
        "a group whose recorded leader is gone must not be signalled: {reset:?}"
    );
    assert!(
        descendant_alive,
        "Reset must not signal a process group whose recorded leader is gone"
    );
}

/// A recycled PID that is alive but does not lead a process group must not keep
/// a Worker `busy` forever: reconciliation uses the same group-based probe.
#[test]
fn reconcile_runs_marks_a_busy_worker_failed_when_its_pid_is_not_a_group_leader() {
    let temporary_directory = TempDir::new().expect("temporary directory");
    let repository = initialize_repository(temporary_directory.path());
    let worker_id = grow_one_worker(&repository);
    let mut unrelated = spawn_non_group_leader();
    let pid = unrelated.id();
    assert_ne!(
        getpgid(Some(unix_pid(pid))).expect("live process group"),
        unix_pid(pid),
        "the helper process must not lead a process group"
    );
    write_worker_json(
        &worker_state_path(temporary_directory.path(), &worker_id),
        busy_run_state(pid, "ENG-232"),
    );

    let snapshot = repository.worker_pool().reconcile_runs();
    let alive = test_kill_process(unix_pid(pid)).is_ok();
    if alive {
        let _ = kill_process(unix_pid(pid), Signal::KILL);
    }
    let _ = unrelated.wait();

    let worker = snapshot
        .worker(worker_id.as_str())
        .expect("reconciled Worker");
    assert!(
        alive,
        "reconciliation must not signal a PID that does not lead a group"
    );
    assert_eq!(
        worker.status(),
        WorkerStatus::Failed,
        "a recycled PID that does not lead a group must not strand a Worker busy"
    );
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
fn reset_run_foreground_helper() {
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
        .reserve_named(worker_id, "ENG-231")
        .expect("Worker reservation");
    let completed = RunSupervisor::new()
        .run_reserved_foreground(
            reservation,
            AgentRuntimeInvocation::new("reset foreground test"),
        )
        .expect("reserved foreground Run should complete");
    fs::write(
        env::var_os(HELPER_EXIT).expect("exit path"),
        format!("exit={:?}", completed.exit_code()),
    )
    .expect("abandoned foreground outcome");
}

#[test]
#[ignore]
fn reserved_foreground_run_helper() {
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
        .reserve_named(worker_id, "ENG-230")
        .expect("Worker reservation");
    if let Some(reserved) = env::var_os(HELPER_RESERVED) {
        fs::write(reserved, []).expect("reservation barrier");
        wait_for_file(&PathBuf::from(
            env::var_os(HELPER_PROCEED).expect("proceed barrier"),
        ));
    }
    let result = env::var_os(HELPER_RESULT).expect("result path");
    match RunSupervisor::new().run_reserved_foreground(
        reservation,
        AgentRuntimeInvocation::new("reserved foreground test"),
    ) {
        Ok(completed) => fs::write(&result, format!("exit={:?}", completed.exit_code()))
            .expect("foreground exit"),
        Err(error) => fs::write(&result, error.to_string()).expect("foreground error"),
    }
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
fn reserved_background_run_cwd_helper() {
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
        .reserve_named(worker_id, "ENG-1")
        .expect("Worker reservation");
    let background = RunSupervisor::new()
        .run_reserved_background(reservation, AgentRuntimeInvocation::new("cwd test"))
        .expect("background Run should start");
    let outcome = background.wait().expect("background Run should complete");
    assert_eq!(outcome.exit_code(), Some(0));
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
        "pi" => {
            AgentRuntime::Pi
                .probe(&workspace)
                .expect("Pi probe should start");
            "ok".to_owned()
        }
        "pi-failed" | "pi-malformed" | "pi-unsupported" | "pi-missing" => {
            let error = AgentRuntime::Pi
                .probe(&workspace)
                .expect_err("Pi probe should reject the fake capability result");
            error.to_string()
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
        state.completed_at = Some(jjfx::engine::WireTimestamp::new("2026-07-30T10:05:00Z"));
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
    change: impl FnOnce(&mut jjfx::engine::WorkerState),
) {
    let state_repository = repository.state_store().worker(worker.clone());
    let loaded = match state_repository.load().expect("Worker state") {
        jjfx::engine::Loaded::Present(versioned) => versioned,
        jjfx::engine::Loaded::Missing => panic!("Worker state should exist"),
    };
    let (mut state, revision) = loaded.into_parts();
    change(&mut state);
    let outcome = state_repository
        .commit(Expected::Match(revision), StateChange::Replace(state))
        .expect("commit Worker state");
    assert!(
        matches!(outcome, jjfx::engine::CommitOutcome::Applied(_)),
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

/// Spawns a live process that deliberately does not lead its own process group,
/// so a plain liveness probe would treat it as a live Run while a group-based
/// probe rejects it.
fn spawn_non_group_leader() -> Child {
    Command::new("sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("non-leader helper process")
}

/// Blocks until the Worker records `pid` for a busy Run, so a Reset in another
/// process targets the launched Run rather than a reservation without a PID.
fn wait_for_busy_pid(repository: &Repository, worker: &WorkerId, pid: u32) {
    let deadline = Instant::now() + HELPER_LIVENESS_TIMEOUT;
    loop {
        let recorded = matches!(
            repository.worker_pool().snapshot().worker(worker.as_str()),
            Some(snapshot) if snapshot.status() == WorkerStatus::Busy && snapshot.pid() == Some(pid)
        );
        if recorded {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Worker {worker} never recorded the launched PID {pid}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn initialize_repository(root: &std::path::Path) -> Repository {
    let output = Command::new("jj")
        .args([
            "--config",
            "signing.behavior=drop",
            "git",
            "init",
            "--no-colocate",
        ])
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
    let deadline = Instant::now() + HELPER_LIVENESS_TIMEOUT;
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

/// A PTY opened for a test, with the slave path a helper needs to acquire it.
struct TestPty {
    master: OwnedFd,
    slave_path: PathBuf,
}

fn open_pty() -> TestPty {
    let master =
        rustix::pty::openpt(rustix::pty::OpenptFlags::RDWR | rustix::pty::OpenptFlags::NOCTTY)
            .expect("PTY master should open");
    let slave_path = rustix::pty::ptsname(&master, Vec::new())
        .expect("PTY slave name")
        .into_string()
        .expect("PTY slave name should be UTF-8");
    rustix::pty::grantpt(&master).expect("PTY should grant the slave");
    rustix::pty::unlockpt(&master).expect("PTY should unlock the slave");
    TestPty {
        master,
        slave_path: PathBuf::from(slave_path),
    }
}

/// Opens the slave without making it this process's controlling terminal: the
/// helper acquires it, because a session leader that owns it is what gives the
/// Run's handover a foreground group to restore.
fn open_pty_slave(slave_path: &Path) -> OwnedFd {
    rustix::fs::open(
        slave_path,
        rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOCTTY,
        rustix::fs::Mode::empty(),
    )
    .expect("PTY slave should open")
}

/// Streams a PTY master on a background thread, so a test can wait for a marker
/// with a deadline instead of blocking forever on a Run that stopped.
struct PtyOutput {
    receiver: Receiver<String>,
    buffer: String,
}

impl PtyOutput {
    fn drain(master: &OwnedFd) -> Self {
        let reader = master.try_clone().expect("PTY master clone");
        let (sender, receiver) = channel();
        thread::spawn(move || {
            let mut bytes = [0u8; 4096];
            while let Ok(read) = rustix::io::read(&reader, &mut bytes) {
                if read == 0 {
                    break;
                }
                if sender
                    .send(String::from_utf8_lossy(&bytes[..read]).into_owned())
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            receiver,
            buffer: String::new(),
        }
    }

    fn wait_for(&mut self, marker: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !self.buffer.contains(marker) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "the terminal never produced {marker:?}: {}",
                self.buffer
            );
            match self.receiver.recv_timeout(remaining) {
                Ok(text) => self.buffer.push_str(&text),
                Err(error) => panic!(
                    "the terminal never produced {marker:?} ({error}): {}",
                    self.buffer
                ),
            }
        }
        self.buffer.clone()
    }

    fn buffer(&self) -> &str {
        &self.buffer
    }
}

fn write_executable(path: &std::path::Path, content: &str) {
    fs::write(path, content).expect("fake runtime executable");
    let mut permissions = fs::metadata(path)
        .expect("fake runtime metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make fake runtime executable");
}
