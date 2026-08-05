use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

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

    let version = Command::new(binary)
        .arg("--version")
        .current_dir(temporary_directory.path())
        .output()
        .expect("wsg should run");
    assert!(version.status.success());
    let expected_version = format!("wsg {}\n", env!("CARGO_PKG_VERSION"));
    assert_eq!(String::from_utf8_lossy(&version.stdout), expected_version);
    assert!(version.stderr.is_empty());
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
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("read-only Worker Pool snapshots available")
    );
    assert!(output.stderr.is_empty());
}
