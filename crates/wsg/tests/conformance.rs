#![cfg(unix)]

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use support::{BinarySpec, ConformanceBinaries};

#[test]
fn selected_binary_captures_exit_status_and_output_channels() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let executable = directory.path().join("fake-go-wsg");
    write_executable(
        &executable,
        "#!/bin/sh\nprintf 'machine-value\\n'\nprintf 'human-message\\n' >&2\nexit 7\n",
    );

    let implementation = BinarySpec::new("go", executable);
    let outcome = implementation.run(directory.path(), &["status"]);

    assert_eq!(outcome.status.code(), Some(7));
    assert_eq!(outcome.stdout, b"machine-value\n");
    assert_eq!(outcome.stderr, b"human-message\n");
}

#[test]
fn conformance_configuration_requires_explicit_go_adapters() {
    let error = ConformanceBinaries::from_explicit(
        Path::new(env!("CARGO_BIN_EXE_wsg")).to_path_buf(),
        None,
        None,
    )
    .expect_err("missing Go adapters must not be treated as conformance");

    assert!(error.to_string().contains("WSG_GO_BINARY"));
    assert!(error.to_string().contains("WSG_GO_TEST_BINARY"));
}

#[test]
#[ignore = "requires WSG_GO_BINARY and WSG_GO_TEST_BINARY"]
fn configured_conformance_adapters_are_runnable() {
    let binaries =
        ConformanceBinaries::from_environment().expect("oracle paths should be configured");
    let directory = tempfile::tempdir().expect("temporary directory should be created");

    let rust = binaries.rust.run(directory.path(), &["--version"]);
    let go = binaries.go.run(directory.path(), &["version"]);
    assert!(rust.status.success(), "Rust wsg failed: {:?}", rust.status);
    assert!(go.status.success(), "Go wsg failed: {:?}", go.status);
    assert!(rust.stderr.is_empty());
    assert!(go.stderr.is_empty());
    assert!(String::from_utf8_lossy(&rust.stdout).starts_with("wsg "));
    assert!(String::from_utf8_lossy(&go.stdout).starts_with("wsg "));
}

#[test]
fn conformance_configuration_keeps_the_two_go_adapters_distinct() {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let go = directory.path().join("go-wsg");
    let go_test = directory.path().join("go-wsg.test");
    write_executable(&go, "#!/bin/sh\nexit 0\n");
    write_executable(&go_test, "#!/bin/sh\nexit 0\n");

    let binaries = ConformanceBinaries::from_explicit(
        Path::new(env!("CARGO_BIN_EXE_wsg")).to_path_buf(),
        Some(go.clone()),
        Some(go_test.clone()),
    )
    .expect("explicit adapters should be accepted");

    assert_eq!(binaries.rust.path(), Path::new(env!("CARGO_BIN_EXE_wsg")));
    assert_eq!(binaries.go.path(), go);
    assert_eq!(binaries.go_test.path(), go_test);
    assert_ne!(binaries.go.path(), binaries.go_test.path());
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).expect("script should be written");
    let mut permissions = fs::metadata(path)
        .expect("script metadata should be readable")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("script should be executable");
}
