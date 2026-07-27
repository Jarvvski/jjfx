use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;
use wsg_core::{
    AgentRuntime, AgentRuntimeCapabilities, AgentRuntimeInvocation, AgentRuntimeProbeError,
};

const HELPER_MODE: &str = "WSG_AGENT_RUNTIME_HELPER_MODE";
const HELPER_PATH: &str = "WSG_AGENT_RUNTIME_HELPER_PATH";
const HELPER_RESULT: &str = "WSG_AGENT_RUNTIME_HELPER_RESULT";
const HELPER_WORKSPACE: &str = "WSG_AGENT_RUNTIME_HELPER_WORKSPACE";

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
