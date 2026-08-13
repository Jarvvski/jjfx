#![cfg(unix)]

mod support;

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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
    run_mixed_pool_mutation_scenario(&binaries.go, &binaries.rust);
    run_mixed_pool_mutation_scenario(&binaries.rust, &binaries.go);
}

fn run_mixed_pool_mutation_scenario(first: &BinarySpec, second: &BinarySpec) {
    let directory = support::local_repository();

    let go_create = first.run(directory.path(), &["pool", "1"]);
    assert_success("Go pool create", &go_create);
    let busy_worker = support::mark_worker_busy(directory.path());

    let barrier = support::LockBarrier::acquire(directory.path());
    let mut blocked = Command::new(second.path())
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
    let blocked_output = support::wait_with_output(blocked, "Rust blocked resize");
    assert_success("Rust blocked resize", &blocked_output);

    let go_resize = Command::new(first.path())
        .args(["pool", "resize", "3"])
        .current_dir(directory.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Go resize should spawn");
    let rust_resize = Command::new(second.path())
        .args(["pool", "resize", "4"])
        .current_dir(directory.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Rust resize should spawn");
    let go_output = support::wait_with_output(go_resize, "Go concurrent resize");
    let rust_output = support::wait_with_output(rust_resize, "Rust concurrent resize");
    assert_success_or_conflict("Go concurrent resize", &go_output);
    assert_success_or_conflict("Rust concurrent resize", &rust_output);
    if !go_output.status.success() {
        let retry = first.run(directory.path(), &["pool", "resize", "3"]);
        assert_success("Go retry resize", &retry);
    }
    if !rust_output.status.success() {
        let retry = second.run(directory.path(), &["pool", "resize", "4"]);
        assert_success("Rust retry resize", &retry);
    }

    let final_status = second.run(directory.path(), &["status"]);
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

    let go_reset = Command::new(first.path())
        .args(["pool", "reset", &busy_worker])
        .current_dir(directory.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Go reset should spawn");
    let rust_reset = Command::new(second.path())
        .args(["pool", "reset", &busy_worker])
        .current_dir(directory.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Rust reset should spawn");
    let go_reset = support::wait_with_output(go_reset, "Go concurrent reset");
    let rust_reset = support::wait_with_output(rust_reset, "Rust concurrent reset");
    assert_success_or_conflict("Go concurrent reset", &go_reset);
    assert_success_or_conflict("Rust concurrent reset", &rust_reset);
    if !go_reset.status.success() {
        let retry = first.run(directory.path(), &["pool", "reset", &busy_worker]);
        assert_success("Go reset retry", &retry);
    }
    if !rust_reset.status.success() {
        let retry = second.run(directory.path(), &["pool", "reset", &busy_worker]);
        assert_success("Rust reset retry", &retry);
    }

    let reset_status = second.run(directory.path(), &["status"]);
    assert_success("Rust status after reset", &reset_status);
    assert!(String::from_utf8_lossy(&reset_status.stdout).contains("idle"));
}

#[test]
#[ignore = "requires WSG_GO_BINARY and WSG_GO_TEST_BINARY"]
fn pool_growth_and_destruction_round_trip_between_each_binary() {
    let binaries =
        ConformanceBinaries::from_environment().expect("oracle paths should be configured");
    run_pool_lifecycle_scenario(&binaries.go, &binaries.rust);
    run_pool_lifecycle_scenario(&binaries.rust, &binaries.go);
}

fn run_pool_lifecycle_scenario(first: &BinarySpec, second: &BinarySpec) {
    let directory = support::local_repository();

    let create = first.run(directory.path(), &["pool", "create", "1"]);
    assert_success("first Pool create", &create);

    let list = second.run(directory.path(), &["pool", "list"]);
    assert_success("second Pool list", &list);
    assert!(String::from_utf8_lossy(&list.stdout).contains("Pool: 1 idle"));

    let resize = second.run(directory.path(), &["pool", "resize", "2"]);
    assert_success("second Pool resize", &resize);

    let status = first.run(directory.path(), &["status"]);
    assert_success("first Pool status", &status);
    assert!(String::from_utf8_lossy(&status.stdout).contains("Pool: 2 idle"));

    let destroy = second.run(directory.path(), &["pool", "destroy"]);
    assert_success("second Pool destroy", &destroy);

    let missing = first.run(directory.path(), &["pool", "list"]);
    assert!(!missing.status.success());
    assert!(missing.stdout.is_empty());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("No pool"));

    let recreate = first.run(directory.path(), &["pool", "1"]);
    assert_success("first Pool recreate", &recreate);
    let destroy = second.run(directory.path(), &["pool", "destroy"]);
    assert_success("second Pool second destroy", &destroy);

    let missing = first.run(directory.path(), &["status"]);
    assert!(!missing.status.success());
    assert!(missing.stdout.is_empty());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("No pool"));
}

#[test]
#[ignore = "requires WSG_GO_BINARY and WSG_GO_TEST_BINARY"]
fn cli_contract_inventory_applies_to_both_binary_adapters() {
    let binaries =
        ConformanceBinaries::from_environment().expect("oracle paths should be configured");
    for binary in [&binaries.go, &binaries.rust] {
        let directory = support::local_repository();
        let root = directory
            .path()
            .canonicalize()
            .expect("Repository root should resolve");

        let version = binary.run(directory.path(), &["version"]);
        assert_success("version", &version);
        assert!(String::from_utf8_lossy(&version.stdout).starts_with("wsg "));
        assert!(version.stderr.is_empty());

        let version_alias = binary.run(directory.path(), &["--version"]);
        assert_success("version alias", &version_alias);
        assert_eq!(version.stdout, version_alias.stdout);
        assert!(version_alias.stderr.is_empty());

        let root_output = binary.run(directory.path(), &["root"]);
        assert_success("root", &root_output);
        assert_eq!(
            root_output.stdout,
            format!("{}\n", root.display()).as_bytes()
        );
        assert!(root_output.stderr.is_empty());

        let where_output = binary.run(directory.path(), &["where"]);
        let info_output = binary.run(directory.path(), &["info"]);
        assert_success("where", &where_output);
        assert_success("info", &info_output);
        assert_eq!(where_output.stdout, info_output.stdout);
        assert_eq!(where_output.stderr, info_output.stderr);

        let add = binary.run(directory.path(), &["a", "contract-feature"]);
        assert_success("add alias", &add);
        assert!(!add.stdout.is_empty());
        assert!(add.stderr.is_empty());
        let list = binary.run(directory.path(), &["ls"]);
        assert_success("list alias", &list);
        assert!(String::from_utf8_lossy(&list.stdout).contains("contract-feature"));
        let remove = binary.run(directory.path(), &["remove", "--force", "contract-feature"]);
        assert_success("remove", &remove);
        assert!(remove.stdout.is_empty());
        assert!(!remove.stderr.is_empty());

        let refresh = binary.run(directory.path(), &["sync"]);
        assert_success("refresh alias", &refresh);
        assert!(refresh.stdout.is_empty());
        assert!(String::from_utf8_lossy(&refresh.stderr).contains("Cache refreshed"));

        let create = binary.run(directory.path(), &["pool", "1"]);
        assert_success("pool create", &create);
        let canonical = binary.run(directory.path(), &["pool", "list"]);
        assert_success("pool list", &canonical);
        for alias in [
            &["pool"][..],
            &["pool", "ls"][..],
            &["pool", "status"][..],
            &["status"][..],
        ] {
            let output = binary.run(directory.path(), alias);
            assert_success("pool status alias", &output);
            assert_eq!(output.stdout, canonical.stdout);
            assert_eq!(output.stderr, canonical.stderr);
        }
        let destroy = binary.run(directory.path(), &["pool", "destroy"]);
        assert_success("pool destroy", &destroy);
        assert!(destroy.stdout.is_empty());

        let completion = binary.run(directory.path(), &["completion", "zsh"]);
        assert_success("completion", &completion);
        assert!(!completion.stdout.is_empty());
        assert!(completion.stderr.is_empty());

        let unsupported = binary.run(directory.path(), &["completion", "bash"]);
        assert!(!unsupported.status.success());
        assert!(unsupported.stdout.is_empty());
        assert!(!unsupported.stderr.is_empty());

        let unknown = binary.run(directory.path(), &["not-a-command"]);
        assert!(!unknown.status.success());
        assert!(unknown.stdout.is_empty());
        assert!(String::from_utf8_lossy(&unknown.stderr).contains("Unknown command"));

        let missing = binary.run(directory.path(), &["path"]);
        assert!(!missing.status.success());
        assert!(missing.stdout.is_empty());
        assert!(String::from_utf8_lossy(&missing.stderr).contains("Usage: wsg path"));
    }
}

#[test]
#[ignore = "requires WSG_GO_BINARY and WSG_GO_TEST_BINARY"]
fn interrupted_pool_and_worker_replacements_leave_cross_implementation_state_valid() {
    let binaries =
        ConformanceBinaries::from_environment().expect("oracle paths should be configured");
    run_interrupted_state_scenario(&binaries.go, &binaries.rust, &binaries.go_test);
    run_interrupted_state_scenario(&binaries.rust, &binaries.go, &binaries.go_test);
}

fn run_interrupted_state_scenario(creator: &BinarySpec, reader: &BinarySpec, go_test: &BinarySpec) {
    let directory = support::local_repository();
    let create = creator.run(directory.path(), &["pool", "1"]);
    assert_success("interrupted Pool create", &create);

    let pool_path = directory.path().join(".jj/pool.json");
    support::add_unknown_field(&pool_path, "future_pool");
    let pool_temp = directory.path().join(".jj/pool.json.tmp-interrupted");
    let pool_writer = support::interrupted_artifact(&pool_temp);
    support::stop_child(pool_writer);
    let resize = reader.run(directory.path(), &["pool", "resize", "2"]);
    assert_success("Pool rewrite after interruption", &resize);
    let pool_document = fs::read_to_string(&pool_path).expect("Pool should remain readable");
    assert!(pool_document.contains("future_pool"));

    let worker = support::first_worker(directory.path());
    let worker_path = directory
        .path()
        .join(".jj/pool")
        .join(format!("{worker}.json"));
    support::add_unknown_field(&worker_path, "future_worker");
    support::mark_worker_busy(directory.path());
    let helper_result = directory.path().join("go-helper-result.json");
    let helper_environment = [
        ("WSG_STATE_HELPER_MODE", OsStr::new("rewrite")),
        ("WSG_STATE_HELPER_KIND", OsStr::new("worker")),
        ("WSG_STATE_HELPER_STATE", worker_path.as_os_str()),
        ("WSG_STATE_HELPER_RESULT", helper_result.as_os_str()),
    ];
    let helper = go_test.run_with_environment(
        directory.path(),
        &["-test.run", "^TestStateLockSubprocessHelper$"],
        &helper_environment,
    );
    assert_success("Go state rewrite helper", &helper);
    assert!(helper_result.exists());
    let helper_document =
        fs::read_to_string(&worker_path).expect("Go helper should rewrite Worker");
    assert!(helper_document.contains("future_worker"));
    let worker_temp = directory
        .path()
        .join(".jj/pool")
        .join(format!("{worker}.json.tmp-interrupted"));
    let worker_writer = support::interrupted_artifact(&worker_temp);
    support::stop_child(worker_writer);
    let reset = reader.run(directory.path(), &["pool", "reset", &worker]);
    assert_success("Worker rewrite after interruption", &reset);
    let worker_document = fs::read_to_string(&worker_path).expect("Worker should remain readable");
    assert!(worker_document.contains("future_worker"));
}

#[test]
#[ignore = "requires WSG_GO_BINARY and WSG_GO_TEST_BINARY"]
fn go_and_rust_share_the_non_cosmetic_cli_contract() {
    let binaries =
        ConformanceBinaries::from_environment().expect("oracle paths should be configured");
    let go_directory = support::local_repository();
    let rust_directory = support::local_repository();

    let go_refresh = binaries.go.run(go_directory.path(), &["refresh"]);
    let rust_refresh = binaries.rust.run(rust_directory.path(), &["refresh"]);
    assert_success("Go refresh", &go_refresh);
    assert_success("Rust refresh", &rust_refresh);
    assert_eq!(go_refresh.stdout, rust_refresh.stdout);
    assert_eq!(go_refresh.stderr, rust_refresh.stderr);

    let go_help = binaries.go.run(go_directory.path(), &["help"]);
    let rust_help = binaries.rust.run(rust_directory.path(), &["help"]);
    assert_success("Go help", &go_help);
    assert_success("Rust help", &rust_help);
    for command in ["wsg add", "wsg pool", "wsg dispatch"] {
        assert!(String::from_utf8_lossy(&go_help.stdout).contains(command));
        assert!(String::from_utf8_lossy(&rust_help.stdout).contains(command));
    }

    let go_unknown = binaries.go.run(go_directory.path(), &["not-a-command"]);
    let rust_unknown = binaries.rust.run(rust_directory.path(), &["not-a-command"]);
    assert_eq!(go_unknown.status.success(), rust_unknown.status.success());
    assert!(go_unknown.stdout.is_empty());
    assert!(rust_unknown.stdout.is_empty());
    assert!(String::from_utf8_lossy(&go_unknown.stderr).contains("Unknown command"));
    assert!(String::from_utf8_lossy(&rust_unknown.stderr).contains("Unknown command"));
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
fn dispatch_group_progress_created_and_resumed_across_implementations() {
    let binaries =
        ConformanceBinaries::from_environment().expect("oracle paths should be configured");
    run_dispatch_group_scenario(&binaries.go, &binaries.rust, "map");
    run_dispatch_group_scenario(&binaries.rust, &binaries.go, "array");
}

fn run_dispatch_group_scenario(creator: &BinarySpec, reconciler: &BinarySpec, graph_shape: &str) {
    let directory = support::local_repository();
    let runtime_directory = directory.path().join("fake-runtime");
    fs::create_dir(&runtime_directory).expect("fake runtime directory should be created");
    write_executable(
        &runtime_directory.join("claude"),
        "#!/bin/sh\ncase \"$*\" in *'--output-format json'*) if [ \"$WSG_GROUP_RUNTIME_SHAPE\" = \"map\" ]; then printf '%s\\n' '{\"sub_issues\":{\"ENG-101\":{\"title\":\"First\",\"status\":\"Todo\",\"blocked_by\":[],\"cross_repo\":false},\"ENG-102\":{\"title\":\"Second\",\"status\":\"Todo\",\"blocked_by\":[\"ENG-101\"],\"cross_repo\":false}}}'; else printf '%s\\n' '{\"sub_issues\":[{\"id\":\"ENG-101\",\"title\":\"First\",\"status\":\"Todo\",\"blocked_by\":[],\"cross_repo\":false},{\"id\":\"ENG-102\",\"title\":\"Second\",\"status\":\"Todo\",\"blocked_by\":[\"ENG-101\"],\"cross_repo\":false}]}'; fi; exit 0;; esac\nif [ \"$1\" = \"--help\" ]; then printf '%s\\n' 'Usage: claude --forward-subagent-text'; exit 0; fi\nprintf '%s\\n' \"$$\" > \"$WSG_GROUP_RUNTIME_PID_FILE\"\nif [ \"$WSG_GROUP_RUNTIME_MODE\" = \"hold\" ]; then while :; do sleep 0.05; done; fi\nprintf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false}'\n",
    );
    let pid_file = directory.path().join("group-runtime.pid");
    let current_path = env::var_os("PATH").expect("PATH should be configured");
    let mut paths = vec![runtime_directory];
    paths.extend(env::split_paths(&current_path));
    let path = env::join_paths(paths).expect("fake runtime PATH should be valid");
    let group_path = directory.path().join(".jj/pool/dispatch-eng-100.json");
    let hold_environment = [
        ("PATH", path.as_os_str()),
        ("WSG_GROUP_RUNTIME_MODE", OsStr::new("hold")),
        ("WSG_GROUP_RUNTIME_SHAPE", OsStr::new(graph_shape)),
        ("WSG_GROUP_RUNTIME_PID_FILE", pid_file.as_os_str()),
    ];

    let create = creator.run_with_environment(directory.path(), &["pool", "2"], &hold_environment);
    assert_success("Dispatch Group Pool create", &create);
    let mut first_run = Some(
        Command::new(creator.path())
            .args(["__orchestrate", "ENG-100"])
            .current_dir(directory.path())
            .envs(hold_environment.iter().copied())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("initial orchestration should spawn"),
    );
    wait_for_child_file(&group_path, &mut first_run);
    support::add_unknown_field(&group_path, "future_group");
    let interrupted_group = directory.path().join(".jj/pool/dispatch-eng-100.json.tmp");
    let interrupted_writer = support::interrupted_artifact(&interrupted_group);
    support::stop_child(interrupted_writer);
    let worker = support::wait_for_assigned_group_worker(directory.path());
    support::wait_for_file(&pid_file);
    let leader = fs::read_to_string(&pid_file)
        .expect("runtime PID should be readable")
        .trim()
        .parse::<u32>()
        .expect("runtime PID should be numeric");
    let process_guard = support::ProcessTreeGuard::new(leader);
    let mut first_run = first_run
        .take()
        .expect("initial orchestration child should remain running");
    first_run.kill().expect("initial orchestration should stop");
    drop(process_guard);
    let _ = support::wait_with_output(first_run, "initial orchestration");
    support::mark_worker_done(directory.path(), &worker, "ENG-101");

    let restart_environment = [
        ("PATH", path.as_os_str()),
        ("WSG_GROUP_RUNTIME_MODE", OsStr::new("complete")),
        ("WSG_GROUP_RUNTIME_SHAPE", OsStr::new(graph_shape)),
        ("WSG_GROUP_RUNTIME_PID_FILE", pid_file.as_os_str()),
    ];
    let restart = Command::new(reconciler.path())
        .args(["__orchestrate", "ENG-100"])
        .current_dir(directory.path())
        .envs(restart_environment.iter().copied())
        .output()
        .expect("resumed orchestration should run");
    let restart_output = support::CommandOutcome::from(restart);
    assert_success("resumed orchestration", &restart_output);
    let (done, failed, skipped, total) = support::dispatch_group_counts(directory.path());
    assert_eq!((failed, skipped), (0, 0));
    assert_eq!(done, total);
    assert_eq!(total, 2);
    let persisted_group = fs::read_to_string(&group_path).expect("Dispatch Group should remain");
    assert!(
        persisted_group.contains("future_group"),
        "Dispatch Group extension field was lost for graph shape {graph_shape}"
    );
    let status = reconciler.run(directory.path(), &["status"]);
    assert_success("resumed Pool status", &status);
    assert!(String::from_utf8_lossy(&status.stdout).contains("idle"));
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
    let competing = reconciler.run_with_input_and_environment(
        directory.path(),
        &["dispatch", "ENG-COMPETING", "--no-orchestrate", "--bg"],
        b"n\n",
        &environment,
    );
    let competing_message = String::from_utf8_lossy(&competing.stderr);
    assert!(
        competing_message.contains("No idle workers")
            || competing_message.contains("No more idle workers"),
        "unexpected contention message: {competing_message}"
    );
    let pool = reconciler.run(directory.path(), &["pool", "list"]);
    assert_success("Reservation pool status", &pool);
    assert!(
        String::from_utf8_lossy(&pool.stdout).contains("(1 total)"),
        "Reservation changed Pool state: {}",
        String::from_utf8_lossy(&pool.stdout)
    );
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

fn wait_for_child_file(path: &Path, child: &mut Option<Child>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        if let Some(status) = child
            .as_mut()
            .expect("orchestration child")
            .try_wait()
            .expect("orchestration status should be readable")
        {
            let output = child
                .take()
                .expect("orchestration child")
                .wait_with_output()
                .expect("orchestration output should be readable");
            panic!(
                "orchestration exited {status} before {}: stdout={} stderr={}",
                path.display(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(20));
    }
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
        {
            let stderr = String::from_utf8_lossy(&output.stderr);
            stderr.contains("conflicted with another process")
                || stderr.contains("persisted state changed")
        },
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
