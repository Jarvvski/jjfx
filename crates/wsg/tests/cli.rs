use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use wsg_core::{
    AgentRuntime, CommitOutcome, Expected, Loaded, PoolCapacity, Repository, StateChange,
    WireAgent, WorkerStatus,
};

fn local_repository() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let output = Command::new("jj")
        .args(["--config", "signing.behavior=drop", "git", "init"])
        .arg(directory.path())
        .output()
        .expect("jj should run");
    assert!(
        output.status.success(),
        "jj init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let remote = Command::new("jj")
        .args(["git", "remote", "add", "origin", "owner/repo"])
        .current_dir(directory.path())
        .output()
        .expect("jj remote add should run");
    assert!(
        remote.status.success(),
        "jj remote add failed: {}",
        String::from_utf8_lossy(&remote.stderr)
    );
    directory
}

fn run(binary: &str, directory: &Path, args: &[&str]) -> std::process::Output {
    Command::new(binary)
        .args(args)
        .current_dir(directory)
        .output()
        .expect("wsg should run")
}

fn set_pool_runtime(repository: &Repository, runtime: AgentRuntime) {
    repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("Pool capacity"))
        .expect("Worker Pool should grow");
    let state_repository = repository.state_store().pool();
    let loaded = match state_repository.load().expect("Pool state") {
        Loaded::Present(versioned) => versioned,
        Loaded::Missing => panic!("Pool state should exist"),
    };
    let (mut state, revision) = loaded.into_parts();
    state.agent = Some(WireAgent::new(runtime.as_str()));
    let outcome = state_repository
        .commit(Expected::Match(revision), StateChange::Replace(state))
        .expect("configured Pool runtime");
    assert!(matches!(outcome, CommitOutcome::Applied(_)));
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).expect("write helper executable");
    let mut permissions = fs::metadata(path).expect("helper metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("helper permissions");
}

fn run_with_input(
    binary: &str,
    directory: &Path,
    args: &[&str],
    input: &[u8],
) -> std::process::Output {
    let mut child = Command::new(binary)
        .args(args)
        .current_dir(directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("wsg should run");
    child
        .stdin
        .take()
        .expect("stdin should be piped")
        .write_all(input)
        .expect("input should be written");
    child.wait_with_output().expect("wsg should finish")
}

#[test]
fn pi_dispatch_all_reports_missing_helper_without_reserving_a_worker() {
    let binary = env!("CARGO_BIN_EXE_wsg");
    let directory = local_repository();
    let repository = Repository::open(directory.path()).expect("repository should open");
    set_pool_runtime(&repository, AgentRuntime::Pi);

    let output = Command::new(binary)
        .args(["dispatch", "--all"])
        .current_dir(directory.path())
        .env("JJFX_PI_LINEAR_HELPER", "")
        .output()
        .expect("wsg should run");

    assert!(!output.status.success());
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostic.contains("JJFX_PI_LINEAR_HELPER"));
    assert!(!diagnostic.contains("claude"));
    assert!(!diagnostic.contains("codex"));
    let snapshot = repository.worker_pool().snapshot();
    assert!(snapshot.workers().iter().all(|worker| {
        worker.status() == WorkerStatus::Idle && worker.ticket().is_none()
    }));
}

#[test]
fn pi_dispatch_all_uses_the_configured_discovery_helper_before_reservation() {
    let binary = env!("CARGO_BIN_EXE_wsg");
    let directory = local_repository();
    let repository = Repository::open(directory.path()).expect("repository should open");
    set_pool_runtime(&repository, AgentRuntime::Pi);
    let helper = directory.path().join("pi-linear-helper");
    let request = directory.path().join("pi-linear-request.json");
    write_executable(
        &helper,
        &format!(
            "#!/bin/sh\ncat > '{}'\nprintf '%s\\n' '{{\"version\":1,\"result\":{{\"tickets\":[]}}}}'\n",
            request.display(),
        ),
    );

    let output = Command::new(binary)
        .args(["dispatch", "--all"])
        .current_dir(directory.path())
        .env("JJFX_PI_LINEAR_HELPER", &helper)
        .output()
        .expect("wsg should run");

    assert!(
        output.status.success(),
        "Pi discovery failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("No tickets found"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            &fs::read_to_string(request).expect("captured helper request")
        )
        .expect("valid helper request"),
        serde_json::json!({
            "version": 1,
            "operation": "ready_tickets",
            "label": "ready-for-agent",
            "status": "Todo",
        }),
    );
    let snapshot = repository.worker_pool().snapshot();
    assert!(snapshot.workers().iter().all(|worker| {
        worker.status() == WorkerStatus::Idle && worker.ticket().is_none()
    }));
}

#[test]
fn help_and_version_work_outside_a_repository() {
    let binary = env!("CARGO_BIN_EXE_wsg");
    let temporary_directory = tempfile::tempdir().expect("temporary directory should be created");

    let help = Command::new(binary)
        .arg("--help")
        .current_dir(temporary_directory.path())
        .output()
        .expect("wsg should run");
    assert!(help.status.success());
    assert!(help.stderr.is_empty());
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage: wsg [OPTIONS]"));

    for argument in ["version", "--version"] {
        let version = Command::new(binary)
            .arg(argument)
            .current_dir(temporary_directory.path())
            .output()
            .expect("wsg should run");
        assert!(version.status.success());
        let expected_version = format!("wsg {}\n", env!("CARGO_PKG_VERSION"));
        assert_eq!(String::from_utf8_lossy(&version.stdout), expected_version);
        assert!(version.stderr.is_empty());
    }
}

#[test]
fn help_documents_workspace_and_pool_command_groups() {
    let binary = env!("CARGO_BIN_EXE_wsg");
    let output = Command::new(binary)
        .arg("help")
        .output()
        .expect("wsg should run");

    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("wsg add <name>"));
    assert!(help.contains("wsg pool <N>"));
    assert!(help.contains("wsg pool destroy"));
    assert!(help.contains("wsg status"));
    assert!(output.stderr.is_empty());
}

#[test]
fn unknown_commands_are_reported_on_stderr() {
    let binary = env!("CARGO_BIN_EXE_wsg");
    let output = Command::new(binary)
        .arg("not-a-command")
        .output()
        .expect("wsg should run");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Unknown command: not-a-command"));
    assert!(output.stdout.is_empty());
}

#[test]
fn repository_commands_keep_paths_on_stdout_and_refresh_on_stderr() {
    let binary = env!("CARGO_BIN_EXE_wsg");
    let directory = local_repository();
    let root = directory
        .path()
        .canonicalize()
        .expect("root should resolve");

    let root_output = run(binary, directory.path(), &["root"]);
    assert!(root_output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&root_output.stdout),
        format!("{}\n", root.display())
    );
    assert!(root_output.stderr.is_empty());

    let path_output = run(binary, directory.path(), &["path", "default"]);
    assert!(path_output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&path_output.stdout),
        format!("{}\n", root.display())
    );
    assert!(path_output.stderr.is_empty());

    let where_output = run(binary, directory.path(), &["where"]);
    assert!(where_output.status.success());
    let where_text = String::from_utf8_lossy(&where_output.stdout);
    assert!(where_text.contains(&format!("repo:       {}", root.display())));
    assert!(where_text.contains("workspaces:"));
    assert!(where_output.stderr.is_empty());

    let refresh_output = run(binary, directory.path(), &["refresh"]);
    assert!(refresh_output.status.success());
    assert!(refresh_output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&refresh_output.stderr),
        "Cache refreshed\n"
    );
}

#[test]
fn workspace_commands_are_compatible_and_clean_requires_explicit_confirmation() {
    let binary = env!("CARGO_BIN_EXE_wsg");
    let directory = local_repository();
    let root = directory
        .path()
        .canonicalize()
        .expect("root should resolve");
    let expected = root.parent().expect("repository parent").join(format!(
        "{}-workspaces/feature",
        root.file_name().expect("repository name").to_string_lossy()
    ));

    let add = run(binary, directory.path(), &["a", "feature"]);
    assert!(add.status.success());
    assert_eq!(
        String::from_utf8_lossy(&add.stdout),
        format!("{}\n", expected.display())
    );
    assert!(add.stderr.is_empty());

    let existing = run(binary, directory.path(), &["add", "feature"]);
    assert!(existing.status.success());
    assert_eq!(
        String::from_utf8_lossy(&existing.stdout),
        format!("{}\n", expected.display())
    );

    let list = run(binary, directory.path(), &["ls"]);
    assert!(list.status.success());
    let list_text = String::from_utf8_lossy(&list.stdout);
    assert!(list_text.contains("  default ➜ "));
    assert!(list_text.contains(&format!("  feature ➜ {}", expected.display())));

    let clean_declined = run_with_input(binary, directory.path(), &["clean"], b"n\n");
    assert!(clean_declined.status.success());
    assert!(expected.is_dir());

    let remove = run(binary, directory.path(), &["remove", "--force", "feature"]);
    assert!(remove.status.success());
    assert!(String::from_utf8_lossy(&remove.stderr).contains("Deleted"));
    assert!(!expected.exists());
}

#[test]
fn pool_commands_create_resize_list_remove_reset_and_destroy() {
    let binary = env!("CARGO_BIN_EXE_wsg");
    let directory = local_repository();

    let create = run(binary, directory.path(), &["pool", "create", "1"]);
    assert!(
        create.status.success(),
        "{}",
        String::from_utf8_lossy(&create.stderr)
    );
    assert!(create.stdout.is_empty());

    let list = run(binary, directory.path(), &["status"]);
    assert!(list.status.success());
    let list_text = String::from_utf8_lossy(&list.stdout);
    assert!(list_text.contains("WORKER"));
    assert!(list_text.contains("Pool: 1 idle"));
    let first_worker = list_text
        .lines()
        .nth(2)
        .and_then(|line| line.split_whitespace().next())
        .expect("status should include a Worker")
        .to_owned();

    let resize = run(binary, directory.path(), &["pool", "r", "--size", "2"]);
    assert!(
        resize.status.success(),
        "{}",
        String::from_utf8_lossy(&resize.stderr)
    );
    assert!(String::from_utf8_lossy(&resize.stderr).contains("expanded"));

    let remove = run(binary, directory.path(), &["pool", "remove", &first_worker]);
    assert!(
        remove.status.success(),
        "{}",
        String::from_utf8_lossy(&remove.stderr)
    );
    assert!(String::from_utf8_lossy(&remove.stderr).contains("Removed worker-"));

    let second_list = run(binary, directory.path(), &["pool", "list"]);
    assert!(second_list.status.success());
    let second_worker = String::from_utf8_lossy(&second_list.stdout)
        .lines()
        .nth(2)
        .and_then(|line| line.split_whitespace().next())
        .expect("remaining Worker should be listed")
        .to_owned();
    let reset = run(binary, directory.path(), &["reset", &second_worker]);
    assert!(
        reset.status.success(),
        "{}",
        String::from_utf8_lossy(&reset.stderr)
    );
    assert!(
        String::from_utf8_lossy(&reset.stderr)
            .contains(&format!("Reset worker-{second_worker} to idle")),
        "{}",
        String::from_utf8_lossy(&reset.stderr)
    );

    let destroy = run(binary, directory.path(), &["pool", "destroy"]);
    assert!(
        destroy.status.success(),
        "{}",
        String::from_utf8_lossy(&destroy.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&destroy.stderr), "Pool destroyed\n");
}

#[test]
fn shell_contract_covers_missing_arguments_and_missing_pool_destroy() {
    let binary = env!("CARGO_BIN_EXE_wsg");
    let directory = local_repository();

    let path = run(binary, directory.path(), &["path"]);
    assert!(!path.status.success());
    assert!(String::from_utf8_lossy(&path.stderr).contains("Usage: wsg path <name>"));
    assert!(path.stdout.is_empty());

    let pool = run(binary, directory.path(), &["pool", "resize"]);
    assert!(!pool.status.success());
    assert!(String::from_utf8_lossy(&pool.stderr).contains("Usage: wsg pool resize <N>"));
    assert!(pool.stdout.is_empty());

    let destroy = run(binary, directory.path(), &["pool", "destroy"]);
    assert!(destroy.status.success());
    assert_eq!(
        String::from_utf8_lossy(&destroy.stderr),
        "No pool to destroy\n"
    );
    assert!(destroy.stdout.is_empty());
}

#[test]
fn status_aliases_share_one_semantic_rendering_path() {
    let binary = env!("CARGO_BIN_EXE_wsg");
    let directory = local_repository();
    let create = run(binary, directory.path(), &["pool", "1"]);
    assert!(create.status.success());

    let canonical = run(binary, directory.path(), &["pool", "list"]);
    assert!(canonical.status.success());
    for args in [
        &["pool"][..],
        &["pool", "ls"][..],
        &["pool", "status"][..],
        &["status"][..],
    ] {
        let output = run(binary, directory.path(), args);
        assert!(output.status.success());
        assert_eq!(output.stdout, canonical.stdout);
        assert_eq!(output.stderr, canonical.stderr);
    }
}

#[test]
fn malformed_pool_state_is_a_nonzero_stderr_failure() {
    let binary = env!("CARGO_BIN_EXE_wsg");
    let directory = local_repository();
    std::fs::write(directory.path().join(".jj/pool.json"), b"not json\n")
        .expect("malformed pool should be written");

    let output = run(binary, directory.path(), &["status"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}

#[test]
fn status_displays_cosmetic_worker_aliases_without_changing_worker_input() {
    let binary = env!("CARGO_BIN_EXE_wsg");
    let directory = local_repository();
    let create = run(binary, directory.path(), &["pool", "1"]);
    assert!(create.status.success());

    let status = run(binary, directory.path(), &["status"]);
    let short_worker = String::from_utf8_lossy(&status.stdout)
        .lines()
        .nth(2)
        .and_then(|line| line.split_whitespace().next())
        .expect("status should include a Worker")
        .to_owned();
    let repository = wsg_core::Repository::open(directory.path()).expect("repository should open");
    let worker = wsg_core::WorkerId::parse(format!("worker-{short_worker}"))
        .expect("Worker ID should be valid");
    repository
        .worker_pool()
        .set_alias(worker, "primary")
        .expect("alias should be stored");

    let aliased = run(binary, directory.path(), &["pool", "ls"]);
    assert!(aliased.status.success());
    let text = String::from_utf8_lossy(&aliased.stdout);
    assert!(text.contains(&short_worker), "{text}");
    assert!(text.contains("primary"), "{text}");
}

#[test]
fn dispatch_and_completion_shell_contracts_are_typed_and_separated() {
    let binary = env!("CARGO_BIN_EXE_wsg");
    let directory = local_repository();

    let missing = run(
        binary,
        directory.path(),
        &["dispatch", "AMBA-42", "--model"],
    );
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("missing value for --model"));

    let completion = run(binary, directory.path(), &["completion", "zsh"]);
    assert!(completion.status.success());
    let script = String::from_utf8_lossy(&completion.stdout);
    assert!(script.contains("__complete non-busy-workers"));
    assert!(!script.contains("__orchestrate"));
    assert!(completion.stderr.is_empty());

    let unsupported = run(binary, directory.path(), &["completion", "bash"]);
    assert!(!unsupported.status.success());
    assert!(String::from_utf8_lossy(&unsupported.stderr).contains("Unsupported shell"));
}

#[test]
fn hidden_completion_is_read_only_outside_a_repository() {
    let binary = env!("CARGO_BIN_EXE_wsg");
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let output = run(binary, directory.path(), &["__complete", "workers"]);
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn no_arguments_report_read_only_pool_capabilities_inside_a_repository() {
    let binary = env!("CARGO_BIN_EXE_wsg");
    let temporary_directory = tempfile::tempdir().expect("temporary directory should be created");
    std::fs::create_dir(temporary_directory.path().join(".jj"))
        .expect("repository marker should be created");

    let output = Command::new(binary)
        .current_dir(temporary_directory.path())
        .output()
        .expect("wsg should run");

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("wsg dispatch"));
    assert!(output.stderr.is_empty());
}
