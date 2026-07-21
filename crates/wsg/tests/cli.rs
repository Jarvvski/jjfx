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
fn no_arguments_report_unimplemented_capabilities_inside_a_repository() {
    let binary = env!("CARGO_BIN_EXE_wsg");
    let temporary_directory = tempfile::tempdir().expect("temporary directory should be created");
    std::fs::create_dir(temporary_directory.path().join(".jj"))
        .expect("repository marker should be created");

    let output = Command::new(binary)
        .current_dir(temporary_directory.path())
        .output()
        .expect("wsg should run");

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("not implemented"));
    assert!(output.stderr.is_empty());
}
