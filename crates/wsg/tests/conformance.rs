#![cfg(unix)]

mod support;

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

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
#[ignore = "requires WSG_GO_BINARY and WSG_GO_TEST_BINARY"]
fn workspaces_created_by_each_binary_are_visible_and_removable_by_the_other() {
    let binaries =
        ConformanceBinaries::from_environment().expect("oracle paths should be configured");
    let directory = support::local_repository();

    let go_add = binaries.go.run(directory.path(), &["add", "go-feature"]);
    assert_success("Go add", &go_add);

    let rust_list = binaries.rust.run(directory.path(), &["list"]);
    assert_success("Rust list", &rust_list);
    assert!(String::from_utf8_lossy(&rust_list.stdout).contains("go-feature"));

    let rust_remove = binaries
        .rust
        .run(directory.path(), &["remove", "--force", "go-feature"]);
    assert_success("Rust remove", &rust_remove);

    let rust_add = binaries
        .rust
        .run(directory.path(), &["add", "rust-feature"]);
    assert_success("Rust add", &rust_add);

    let go_list = binaries.go.run(directory.path(), &["ls"]);
    assert_success("Go list", &go_list);
    assert!(String::from_utf8_lossy(&go_list.stdout).contains("rust-feature"));

    let go_remove = binaries
        .go
        .run(directory.path(), &["rm", "--force", "rust-feature"]);
    assert_success("Go remove", &go_remove);

    let final_list = binaries.rust.run(directory.path(), &["list"]);
    assert_success("Rust final list", &final_list);
    let final_output = String::from_utf8_lossy(&final_list.stdout);
    assert!(!final_output.contains("go-feature"));
    assert!(!final_output.contains("rust-feature"));
}

#[test]
#[ignore = "requires WSG_GO_BINARY and WSG_GO_TEST_BINARY"]
fn mixed_pool_mutations_wait_for_the_shared_lock_and_keep_state_valid() {
    let binaries =
        ConformanceBinaries::from_environment().expect("oracle paths should be configured");
    let directory = support::local_repository();

    let go_create = binaries.go.run(directory.path(), &["pool", "1"]);
    assert_success("Go pool create", &go_create);
    let busy_worker = support::mark_worker_busy(directory.path());

    let barrier = support::LockBarrier::acquire(directory.path());
    let mut blocked = Command::new(binaries.rust.path())
        .args(["pool", "resize", "2"])
        .current_dir(directory.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Rust resize should spawn");
    thread::sleep(Duration::from_millis(150));
    assert!(
        blocked
            .try_wait()
            .expect("Rust resize status should be readable")
            .is_none(),
        "Rust resize bypassed the shared Pool lock"
    );

    barrier.release();
    let blocked_output = support::CommandOutcome::from(
        blocked
            .wait_with_output()
            .expect("Rust resize should finish after lock release"),
    );
    assert_success("Rust blocked resize", &blocked_output);

    let go_resize = Command::new(binaries.go.path())
        .args(["pool", "resize", "3"])
        .current_dir(directory.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Go resize should spawn");
    let rust_resize = Command::new(binaries.rust.path())
        .args(["pool", "resize", "4"])
        .current_dir(directory.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Rust resize should spawn");
    let go_output = support::CommandOutcome::from(
        go_resize
            .wait_with_output()
            .expect("Go concurrent resize should finish"),
    );
    let rust_output = support::CommandOutcome::from(
        rust_resize
            .wait_with_output()
            .expect("Rust concurrent resize should finish"),
    );
    assert_success_or_conflict("Go concurrent resize", &go_output);
    assert_success_or_conflict("Rust concurrent resize", &rust_output);
    if !go_output.status.success() {
        let retry = binaries.go.run(directory.path(), &["pool", "resize", "3"]);
        assert_success("Go retry resize", &retry);
    }
    if !rust_output.status.success() {
        let retry = binaries
            .rust
            .run(directory.path(), &["pool", "resize", "4"]);
        assert_success("Rust retry resize", &retry);
    }

    let final_status = binaries.rust.run(directory.path(), &["status"]);
    assert_success("Rust final status", &final_status);
    let status = String::from_utf8_lossy(&final_status.stdout);
    let pool_line = status
        .lines()
        .find(|line| line.starts_with("Pool:"))
        .expect("final status should include pool totals");
    assert!(pool_line.contains("total)"));
    let worker_names = status
        .lines()
        .skip(2)
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_whitespace().next())
        .collect::<std::collections::BTreeSet<_>>();
    let total = pool_line
        .split_whitespace()
        .find_map(|value| {
            value
                .strip_prefix('(')
                .and_then(|value| value.parse::<usize>().ok())
        })
        .expect("pool total should be numeric");
    assert_eq!(worker_names.len(), total);
    assert!((3..=4).contains(&total));

    let go_reset = Command::new(binaries.go.path())
        .args(["pool", "reset", &busy_worker])
        .current_dir(directory.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Go reset should spawn");
    let rust_reset = Command::new(binaries.rust.path())
        .args(["pool", "reset", &busy_worker])
        .current_dir(directory.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Rust reset should spawn");
    let go_reset =
        support::CommandOutcome::from(go_reset.wait_with_output().expect("Go reset should finish"));
    let rust_reset = support::CommandOutcome::from(
        rust_reset
            .wait_with_output()
            .expect("Rust reset should finish"),
    );
    assert_success_or_conflict("Go concurrent reset", &go_reset);
    assert_success_or_conflict("Rust concurrent reset", &rust_reset);
    if !go_reset.status.success() {
        let retry = binaries
            .go
            .run(directory.path(), &["pool", "reset", &busy_worker]);
        assert_success("Go reset retry", &retry);
    }
    if !rust_reset.status.success() {
        let retry = binaries
            .rust
            .run(directory.path(), &["pool", "reset", &busy_worker]);
        assert_success("Rust reset retry", &retry);
    }

    let reset_status = binaries.rust.run(directory.path(), &["status"]);
    assert_success("Rust status after reset", &reset_status);
    assert!(String::from_utf8_lossy(&reset_status.stdout).contains("idle"));
}

#[test]
#[ignore = "requires WSG_GO_BINARY and WSG_GO_TEST_BINARY"]
fn pool_growth_and_destruction_round_trip_between_each_binary() {
    let binaries =
        ConformanceBinaries::from_environment().expect("oracle paths should be configured");
    let directory = support::local_repository();

    let go_create = binaries.go.run(directory.path(), &["pool", "create", "1"]);
    assert_success("Go pool create", &go_create);

    let rust_list = binaries.rust.run(directory.path(), &["pool", "list"]);
    assert_success("Rust pool list", &rust_list);
    assert!(String::from_utf8_lossy(&rust_list.stdout).contains("Pool: 1 idle"));

    let rust_resize = binaries
        .rust
        .run(directory.path(), &["pool", "resize", "2"]);
    assert_success("Rust pool resize", &rust_resize);

    let go_status = binaries.go.run(directory.path(), &["status"]);
    assert_success("Go pool status", &go_status);
    assert!(String::from_utf8_lossy(&go_status.stdout).contains("Pool: 2 idle"));

    let rust_destroy = binaries.rust.run(directory.path(), &["pool", "destroy"]);
    assert_success("Rust pool destroy", &rust_destroy);

    let go_missing = binaries.go.run(directory.path(), &["pool", "list"]);
    assert!(!go_missing.status.success());
    assert!(go_missing.stdout.is_empty());
    assert!(String::from_utf8_lossy(&go_missing.stderr).contains("No pool"));

    let go_recreate = binaries.go.run(directory.path(), &["pool", "1"]);
    assert_success("Go pool recreate", &go_recreate);
    let rust_destroy = binaries.rust.run(directory.path(), &["pool", "destroy"]);
    assert_success("Rust second pool destroy", &rust_destroy);

    let rust_missing = binaries.rust.run(directory.path(), &["status"]);
    assert!(!rust_missing.status.success());
    assert!(rust_missing.stdout.is_empty());
    assert!(String::from_utf8_lossy(&rust_missing.stderr).contains("No pool"));
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

#[test]
#[ignore = "requires WSG_GO_BINARY and WSG_GO_TEST_BINARY"]
fn runtime_process_groups_are_reconciled_across_implementations() {
    let binaries =
        ConformanceBinaries::from_environment().expect("oracle paths should be configured");
    run_runtime_scenario(&binaries.go, &binaries.rust);
    run_runtime_scenario(&binaries.rust, &binaries.go);
}

fn run_runtime_scenario(creator: &BinarySpec, reconciler: &BinarySpec) {
    let directory = support::local_repository();
    let runtime_directory = directory.path().join("fake-runtime");
    fs::create_dir(&runtime_directory).expect("fake runtime directory should be created");
    let runtime = runtime_directory.join("claude");
    write_executable(
        &runtime,
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then printf '%s\\n' 'Usage: claude --forward-subagent-text'; exit 0; fi\nprintf '%s\\n' \"$$\" > \"$WSG_RUNTIME_LEADER_PID_FILE\"\nsleep 30 &\nprintf '%s\\n' \"$!\" > \"$WSG_RUNTIME_DESCENDANT_PID_FILE\"\nwhile :; do sleep 0.05; done\n",
    );
    let leader_pid = directory.path().join("leader.pid");
    let descendant_pid = directory.path().join("descendant.pid");
    let current_path = env::var_os("PATH").expect("PATH should be configured");
    let mut paths = vec![runtime_directory];
    paths.extend(env::split_paths(&current_path));
    let path = env::join_paths(paths).expect("fake runtime PATH should be valid");
    let environment = [
        ("PATH", path.as_os_str()),
        ("WSG_RUNTIME_LEADER_PID_FILE", leader_pid.as_os_str()),
        (
            "WSG_RUNTIME_DESCENDANT_PID_FILE",
            descendant_pid.as_os_str(),
        ),
    ];

    let create = creator.run_with_environment(directory.path(), &["pool", "1"], &environment);
    assert_success("runtime Pool create", &create);
    let dispatch = creator.run_with_environment(
        directory.path(),
        &["dispatch", "ENG-CONFORMANCE", "--no-orchestrate", "--bg"],
        &environment,
    );
    assert_success("runtime dispatch", &dispatch);
    support::wait_for_file(&leader_pid);
    support::wait_for_file(&descendant_pid);

    let worker = support::first_worker(directory.path());
    let recorded_pid = support::recorded_worker_pid(directory.path(), &worker);
    let _process_guard = support::ProcessTreeGuard::new(recorded_pid);
    let descendant = fs::read_to_string(&descendant_pid)
        .expect("descendant PID should be readable")
        .trim()
        .parse::<u32>()
        .expect("descendant PID should be numeric");

    let reset = reconciler.run(directory.path(), &["pool", "reset", &worker]);
    assert_success("cross-implementation runtime reset", &reset);
    support::wait_for_process_exit(recorded_pid);
    support::wait_for_process_exit(descendant);
    let status = reconciler.run(directory.path(), &["status"]);
    assert_success("cross-implementation runtime status", &status);
    assert!(String::from_utf8_lossy(&status.stdout).contains("idle"));
}

#[test]
#[ignore = "helper invoked by mixed conformance tests"]
fn conformance_lock_helper() {
    support::run_lock_helper();
}

fn assert_success(label: &str, output: &support::CommandOutcome) {
    assert!(
        output.status.success(),
        "{label} failed with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_success_or_conflict(label: &str, output: &support::CommandOutcome) {
    if output.status.success() {
        return;
    }
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("conflicted with another process"),
        "{label} failed unexpectedly with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).expect("script should be written");
    let mut permissions = fs::metadata(path)
        .expect("script metadata should be readable")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("script should be executable");
}
