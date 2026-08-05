use std::process::Command;

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
