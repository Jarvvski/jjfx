use std::env;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;
use wsg_core::{AgentRuntime, Repository, SnapshotDiagnosticKind, WorkerStatus};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/compatibility");
fn fixture(name: &str) -> Vec<u8> {
    fs::read(Path::new(FIXTURES).join(name)).expect("fixture")
}

fn workspace_names(root: &Path) -> Vec<String> {
    let output = Command::new("jj")
        .args(["workspace", "list"])
        .current_dir(root)
        .output()
        .expect("jj workspace list");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once(':').map(|(name, _)| name.trim().to_owned()))
        .collect()
}

#[test]
#[ignore]
fn stale_reservation_after_destroy_helper() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    let worker = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect("grow")
        .added_workers()[0]
        .clone();
    let reservation = pool
        .reserve_named(worker.clone(), "AMBA-STALE")
        .expect("reserve Worker");
    let repo_state = repository.root().join(".jj/repo");
    let disabled = repository.root().join(".jj/repo-disabled");
    fs::rename(&repo_state, &disabled).expect("disable jj repository");
    pool.destroy()
        .expect_err("destroy should stop after detaching membership");
    fs::rename(&disabled, &repo_state).expect("restore jj repository");

    let workspace = repository
        .root()
        .parent()
        .expect("repository parent")
        .join(format!(
            "{}-workspaces/{worker}",
            repository
                .root()
                .file_name()
                .expect("repository name")
                .to_string_lossy()
        ));
    fs::create_dir_all(&workspace).expect("recreate detached Workspace path");
    let error = wsg_core::RunSupervisor::new()
        .run_reserved_background(
            &reservation,
            wsg_core::AgentRuntimeInvocation::new("stale reservation"),
        )
        .expect_err("detached Reservation must not persist a launched PID");
    assert!(
        matches!(
            error,
            wsg_core::RunSupervisorError::PersistPidConflict { worker: ref rejected }
                if rejected == &worker
        ),
        "unexpected stale Reservation error: {error:?}"
    );
    let wsg_core::Loaded::Present(state) = repository
        .state_store()
        .worker(worker)
        .load()
        .expect("Worker state")
    else {
        panic!("detached Worker state should remain for destroy recovery");
    };
    assert_eq!(state.value.pid, None);
    pool.destroy().expect("destroy should resume");
}

#[test]
#[ignore]
fn destroy_live_process_helper() {
    let pid_path = env::var_os("WSG_DESTROY_PID").expect("PID path");
    let mut child = Command::new("sh")
        .args(["-c", "trap 'exit 0' TERM; while :; do sleep 0.05; done"])
        .process_group(0)
        .spawn()
        .expect("spawn live Run");
    fs::write(pid_path, child.id().to_string()).expect("write live PID");
    let status = child.wait().expect("wait for destroyed Run");
    assert!(status.success(), "Run should handle TERM successfully");
}

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn local_repository_with_origin() -> (TempDir, Repository) {
    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let output = Command::new("jj")
        .args(["--config", "signing.behavior=drop", "git", "init"])
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
    (temporary_directory, repository)
}

fn go_repository_with_pool() -> (TempDir, Repository) {
    let (temp, repository) = local_repository_with_origin();
    let pool = temp.path().join(".jj/pool");
    fs::create_dir_all(&pool).expect("pool directory");
    fs::write(
        temp.path().join(".jj/pool.json"),
        fixture("pool-workers.json"),
    )
    .expect("pool state");
    for (id, name) in [
        ("worker-01", "worker-idle-claude.json"),
        ("worker-02", "worker-busy-claude.json"),
        ("worker-03", "worker-done-codex.json"),
        ("worker-04", "worker-failed-codex.json"),
    ] {
        fs::write(pool.join(format!("{id}.json")), fixture(name)).expect("Worker state");
    }
    (temp, repository)
}

fn repository_with_pool() -> (TempDir, Repository) {
    let temp = tempfile::tempdir().expect("temp repository");
    let pool = temp.path().join(".jj/pool");
    fs::create_dir_all(&pool).expect("pool directory");
    fs::write(
        temp.path().join(".jj/pool.json"),
        fixture("pool-workers.json"),
    )
    .expect("pool state");
    for (id, name) in [
        ("worker-01", "worker-idle-claude.json"),
        ("worker-02", "worker-busy-claude.json"),
        ("worker-03", "worker-done-codex.json"),
        ("worker-04", "worker-failed-codex.json"),
    ] {
        fs::write(pool.join(format!("{id}.json")), fixture(name)).expect("Worker state");
    }
    let repository = Repository::open(temp.path()).expect("repository");
    (temp, repository)
}

#[test]
fn pool_capacity_accepts_an_empty_pool() {
    let capacity = wsg_core::PoolCapacity::new(0).expect("zero capacity should be valid");

    assert_eq!(capacity.as_usize(), 0);
    #[cfg(target_pointer_width = "64")]
    assert!(wsg_core::PoolCapacity::new(usize::MAX).is_err());
}

#[test]
fn stale_reservation_cannot_persist_a_pid_after_destroy_detaches_membership() {
    use std::os::unix::fs::PermissionsExt;

    let temporary_directory = tempfile::tempdir().expect("temporary directory");
    let bin = temporary_directory.path().join("bin");
    fs::create_dir(&bin).expect("runtime bin");
    let runtime = bin.join("claude");
    fs::write(
        &runtime,
        "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nexit 0\n",
    )
    .expect("fake runtime");
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).expect("runtime permissions");
    let mut paths = vec![bin.clone()];
    paths.extend(env::split_paths(
        &env::var_os("PATH").expect("test process PATH"),
    ));
    let path = env::join_paths(paths).expect("runtime PATH");
    let mut child = Command::new(env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "stale_reservation_after_destroy_helper",
            "--ignored",
        ])
        .env("PATH", path)
        .spawn()
        .expect("spawn stale Reservation helper");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().expect("helper status") {
            assert!(
                status.success(),
                "stale Reservation helper failed: {status}"
            );
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("stale Reservation helper timed out");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn destroy_is_idempotent_for_a_missing_or_empty_pool() {
    let (_temporary_directory, repository) = local_repository_with_origin();

    repository
        .worker_pool()
        .destroy()
        .expect("missing Pool destroy should succeed");
    repository
        .worker_pool()
        .destroy()
        .expect("repeated destroy should succeed");

    assert!(repository.worker_pool().snapshot().is_missing());
    assert!(repository.root().join(".jj/pool/.dispatch.lock").exists());
}

#[test]
fn destroy_removes_worker_workspaces_state_and_cache_entries() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let grown = repository
        .worker_pool()
        .resize_to(wsg_core::PoolCapacity::new(2).expect("capacity"))
        .expect("grow");
    let workers = grown.added_workers().to_vec();
    let workspace_paths = workers
        .iter()
        .map(|worker| {
            repository
                .root()
                .parent()
                .expect("repository parent")
                .join(format!(
                    "{}-workspaces/{worker}",
                    repository
                        .root()
                        .file_name()
                        .expect("repository name")
                        .to_string_lossy()
                ))
        })
        .collect::<Vec<_>>();

    repository.worker_pool().destroy().expect("destroy Pool");

    assert!(repository.worker_pool().snapshot().is_missing());
    for (worker, workspace) in workers.iter().zip(workspace_paths) {
        assert!(!workspace.exists());
        assert!(
            !repository
                .root()
                .join(".jj/pool")
                .join(format!("{worker}.json"))
                .exists()
        );
        let output = Command::new("jj")
            .args(["workspace", "list"])
            .current_dir(repository.root())
            .output()
            .expect("jj workspace list");
        assert!(!String::from_utf8_lossy(&output.stdout).contains(worker.as_str()));
    }
    let cache = fs::read_to_string(repository.root().join(".jj/ws-cache")).expect("ws-cache");
    assert!(
        workers
            .iter()
            .all(|worker| !cache.contains(worker.as_str()))
    );
}

#[test]
fn destroy_removes_dispatch_and_unknown_artifacts_but_preserves_lock_sidecars() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let worker = repository
        .worker_pool()
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect("grow")
        .added_workers()[0]
        .clone();
    let parent = wsg_core::TicketId::parse("AMBA-99").expect("Ticket ID");
    let group = wsg_core::DispatchGroupState::new(
        parent.clone(),
        wsg_core::WireTimestamp::new("2026-07-30T10:00:00Z"),
        "owner/repo",
        wsg_core::DispatchGroupOptions::new(""),
    );
    repository
        .state_store()
        .dispatch_group(parent)
        .commit(
            wsg_core::Expected::Missing,
            wsg_core::StateChange::Replace(group),
        )
        .expect("Dispatch Group state");
    let pool_directory = repository.root().join(".jj/pool");
    fs::write(pool_directory.join("unknown.tmp"), "unknown").expect("unknown file");
    fs::create_dir(pool_directory.join("unknown-dir")).expect("unknown directory");
    fs::write(pool_directory.join("unknown-dir/value"), "unknown").expect("unknown value");

    repository.worker_pool().destroy().expect("destroy Pool");

    for artifact in [
        repository.root().join(".jj/pool.json"),
        pool_directory.join(".destroying"),
        pool_directory.join(format!("{worker}.cleanup")),
        pool_directory.join("dispatch-amba-99.json"),
        pool_directory.join("unknown.tmp"),
        pool_directory.join("unknown-dir"),
    ] {
        assert!(
            !artifact.exists(),
            "{} should be removed",
            artifact.display()
        );
    }
    for lock in [
        pool_directory.join(".dispatch.lock"),
        pool_directory.join(format!("{worker}.json.lock")),
        pool_directory.join("dispatch-amba-99.json.lock"),
        repository.root().join(".jj/ws-cache.lock"),
    ] {
        assert!(lock.exists(), "{} should remain", lock.display());
    }
}

#[test]
fn destroy_marker_blocks_growth_until_destroy_completes() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    pool.resize_to(wsg_core::PoolCapacity::new(0).expect("empty capacity"))
        .expect("create empty Pool");
    fs::write(repository.root().join(".jj/pool/.destroying"), "")
        .expect("interrupted destroy marker");

    let error = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect_err("growth must not commit during destroy");

    assert!(matches!(error, wsg_core::WorkerPoolError::Conflict));
    assert_eq!(pool.snapshot().pool().expect("empty Pool").size(), 0);
    pool.destroy()
        .expect("destroy should remove its durable marker");
    assert!(!repository.root().join(".jj/pool/.destroying").exists());

    let growth = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect("growth should resume after destroy completes");
    assert_eq!(growth.capacity().as_usize(), 1);
}

#[test]
fn repeated_destroy_retries_interrupted_workspace_cleanup() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let worker = repository
        .worker_pool()
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect("grow")
        .added_workers()[0]
        .clone();
    let repo_state = repository.root().join(".jj/repo");
    let disabled = repository.root().join(".jj/repo-disabled");
    fs::rename(&repo_state, &disabled).expect("disable jj repository");

    let error = repository
        .worker_pool()
        .destroy()
        .expect_err("Workspace cleanup should fail after detachment");

    assert!(matches!(
        error,
        wsg_core::WorkerPoolError::DestroyCleanup { ref workers, .. }
            if workers == &vec![worker.clone()]
    ));
    assert_eq!(
        repository
            .worker_pool()
            .snapshot()
            .pool()
            .expect("Pool")
            .size(),
        0
    );
    assert!(
        repository
            .root()
            .join(".jj/pool")
            .join(format!("{worker}.json"))
            .exists()
    );
    assert!(
        repository
            .root()
            .join(".jj/pool")
            .join(format!("{worker}.cleanup"))
            .exists()
    );
    fs::rename(&disabled, &repo_state).expect("restore jj repository");

    repository
        .worker_pool()
        .destroy()
        .expect("repeated destroy should resume cleanup");

    assert!(repository.worker_pool().snapshot().is_missing());
    assert!(
        !repository
            .root()
            .join(".jj/pool")
            .join(format!("{worker}.cleanup"))
            .exists()
    );
}

#[test]
fn destroy_accepts_busy_workers_with_dead_or_absent_pids() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let grown = repository
        .worker_pool()
        .resize_to(wsg_core::PoolCapacity::new(2).expect("capacity"))
        .expect("grow");
    let first = grown.added_workers()[0].clone();
    let second = grown.added_workers()[1].clone();
    repository
        .worker_pool()
        .reserve_named(first.clone(), "AMBA-DEAD")
        .expect("reserve first");
    repository
        .worker_pool()
        .reserve_named(second, "AMBA-NO-PID")
        .expect("reserve second");
    let state_repository = repository.state_store().worker(first);
    let wsg_core::Loaded::Present(loaded) = state_repository.load().expect("Worker state") else {
        panic!("Worker state should exist");
    };
    let (mut state, revision) = loaded.into_parts();
    state.pid = Some(99_999_999);
    state_repository
        .commit(
            wsg_core::Expected::Match(revision),
            wsg_core::StateChange::Replace(state),
        )
        .expect("persist dead PID");

    repository
        .worker_pool()
        .destroy()
        .expect("dead and missing PIDs should not block destroy");

    assert!(repository.worker_pool().snapshot().is_missing());
}

#[test]
fn destroy_terminates_a_live_recorded_process_group() {
    let (temporary_directory, repository) = local_repository_with_origin();
    let worker = repository
        .worker_pool()
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect("grow")
        .added_workers()[0]
        .clone();
    repository
        .worker_pool()
        .reserve_named(worker.clone(), "AMBA-LIVE")
        .expect("reserve Worker");
    let pid_path = temporary_directory.path().join("live-pid");
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args(["--exact", "destroy_live_process_helper", "--ignored"])
        .env("WSG_DESTROY_PID", &pid_path);
    let helper = helper.spawn().expect("spawn process owner");
    wait_for_file(&pid_path);
    let pid = fs::read_to_string(&pid_path)
        .expect("read PID")
        .parse::<i64>()
        .expect("numeric PID");
    let state_repository = repository.state_store().worker(worker);
    let wsg_core::Loaded::Present(loaded) = state_repository.load().expect("Worker state") else {
        panic!("Worker state should exist");
    };
    let (mut state, revision) = loaded.into_parts();
    state.pid = Some(pid);
    state_repository
        .commit(
            wsg_core::Expected::Match(revision),
            wsg_core::StateChange::Replace(state),
        )
        .expect("persist live PID");

    repository
        .worker_pool()
        .destroy()
        .expect("destroy should terminate the live Run");

    let output = helper.wait_with_output().expect("wait for process owner");
    assert!(
        output.status.success(),
        "process owner failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(repository.worker_pool().snapshot().is_missing());
}

#[test]
fn creates_a_pool_with_one_worker_visible_through_its_snapshot() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();

    let growth = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity should be valid"))
        .expect("pool should grow");

    assert_eq!(growth.added_workers().len(), 1);
    assert_eq!(growth.capacity().as_usize(), 1);
    let worker = &growth.added_workers()[0];
    assert!(worker.as_str().starts_with("worker-"));
    assert_eq!(worker.as_str().len(), "worker-".len() + 6);

    let snapshot = pool.snapshot();
    let manifest = snapshot.pool().expect("pool manifest");
    assert_eq!(manifest.size(), 1);
    assert_eq!(manifest.gh_repo(), "owner/repo");
    assert_eq!(manifest.workers().len(), 1);
    assert_eq!(snapshot.workers().len(), 1);
    assert_eq!(
        snapshot.worker(worker.as_str()).expect("worker").status(),
        WorkerStatus::Idle
    );
    assert!(snapshot.diagnostics().is_empty());
}

#[test]
fn grows_a_pool_without_changing_existing_worker_ids_or_metadata() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    let first = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity should be valid"))
        .expect("initial pool should grow");
    let existing = first.added_workers()[0].clone();

    let second = pool
        .resize_to(wsg_core::PoolCapacity::new(3).expect("capacity should be valid"))
        .expect("pool should grow again");

    assert_eq!(second.capacity().as_usize(), 3);
    assert_eq!(second.added_workers().len(), 2);
    let snapshot = pool.snapshot();
    let manifest = snapshot.pool().expect("pool manifest");
    assert_eq!(manifest.size(), 3);
    assert_eq!(manifest.gh_repo(), "owner/repo");
    assert_eq!(manifest.workers()[0].worker_id(), &existing);
    assert_eq!(snapshot.workers().len(), 3);
    assert!(snapshot.diagnostics().is_empty());
}

#[test]
fn growing_to_current_capacity_does_not_change_the_pool() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    let first = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity should be valid"))
        .expect("initial pool should grow");
    let before = fs::read(repository.root().join(".jj/pool.json")).expect("pool bytes");

    let second = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity should be valid"))
        .expect("same capacity should be accepted");

    assert!(second.added_workers().is_empty());
    assert_eq!(second.capacity().as_usize(), 1);
    assert_eq!(second.capacity(), first.capacity());
    assert_eq!(
        fs::read(repository.root().join(".jj/pool.json")).expect("pool bytes"),
        before
    );
}

#[test]
fn shrinking_removes_the_stable_pool_tail_and_reports_it() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    let grown = pool
        .resize_to(wsg_core::PoolCapacity::new(3).expect("capacity should be valid"))
        .expect("initial pool should grow");
    let expected_removed = grown.added_workers()[1..].to_vec();

    let resized = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity should be valid"))
        .expect("idle tail should shrink");

    assert_eq!(resized.capacity().as_usize(), 1);
    assert!(resized.added_workers().is_empty());
    assert_eq!(resized.removed_workers(), expected_removed);
    let snapshot = pool.snapshot();
    assert_eq!(snapshot.pool().expect("pool manifest").size(), 1);
    assert_eq!(snapshot.workers().len(), 1);
    for worker in resized.removed_workers() {
        assert!(
            !repository
                .root()
                .join(".jj/pool")
                .join(format!("{worker}.json"))
                .exists()
        );
    }
}

#[test]
fn pool_can_shrink_to_zero_and_regrow_with_new_stable_workers() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    let initial = pool
        .resize_to(wsg_core::PoolCapacity::new(2).expect("capacity"))
        .expect("grow");
    let old_workers = initial.added_workers().to_vec();

    let empty = pool
        .resize_to(wsg_core::PoolCapacity::new(0).expect("zero capacity"))
        .expect("shrink to empty");
    let regrown = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect("regrow");

    assert_eq!(empty.capacity().as_usize(), 0);
    assert_eq!(empty.removed_workers(), old_workers);
    assert_eq!(regrown.capacity().as_usize(), 1);
    assert_eq!(regrown.added_workers().len(), 1);
    assert!(!old_workers.contains(&regrown.added_workers()[0]));
}

#[test]
fn shrinking_rejects_the_whole_tail_when_a_selected_worker_is_busy() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    let grown = pool
        .resize_to(wsg_core::PoolCapacity::new(3).expect("capacity"))
        .expect("grow");
    let busy = grown.added_workers()[2].clone();
    pool.reserve_named(busy.clone(), "AMBA-7")
        .expect("tail Worker should reserve");
    let before = fs::read(repository.root().join(".jj/pool.json")).expect("pool bytes");

    let error = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect_err("busy tail should reject shrink");

    assert!(matches!(
        error,
        wsg_core::WorkerPoolError::WorkersBusy { workers } if workers == vec![busy]
    ));
    assert_eq!(
        fs::read(repository.root().join(".jj/pool.json")).expect("pool bytes"),
        before
    );
    assert_eq!(pool.snapshot().pool().expect("manifest").size(), 3);
}

#[test]
fn named_removal_detaches_an_idle_worker_without_reordering_the_rest() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    let grown = pool
        .resize_to(wsg_core::PoolCapacity::new(3).expect("capacity"))
        .expect("grow");
    let removed = grown.added_workers()[1].clone();
    let expected = vec![
        grown.added_workers()[0].clone(),
        grown.added_workers()[2].clone(),
    ];

    let resized = pool.remove(removed.clone()).expect("remove middle Worker");

    assert_eq!(resized.capacity().as_usize(), 2);
    assert_eq!(resized.removed_workers(), &[removed]);
    let workers = pool
        .snapshot()
        .pool()
        .expect("manifest")
        .workers()
        .iter()
        .map(|worker| worker.worker_id().clone())
        .collect::<Vec<_>>();
    assert_eq!(workers, expected);
}

#[test]
fn named_removal_rejects_unknown_and_busy_workers_without_mutation() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    let grown = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect("grow");
    let busy = grown.added_workers()[0].clone();
    pool.reserve_named(busy.clone(), "AMBA-8").expect("reserve");
    let before = fs::read(repository.root().join(".jj/pool.json")).expect("pool bytes");

    let busy_error = pool
        .remove(busy.clone())
        .expect_err("busy Worker should not be removed");
    let unknown = wsg_core::WorkerId::parse("worker-unknown").expect("Worker ID");
    let unknown_error = pool
        .remove(unknown.clone())
        .expect_err("unknown Worker should not be removed");

    assert!(matches!(
        busy_error,
        wsg_core::WorkerPoolError::WorkersBusy { workers } if workers == vec![busy]
    ));
    assert!(matches!(
        unknown_error,
        wsg_core::WorkerPoolError::WorkerNotInPool { worker } if worker == unknown
    ));
    assert_eq!(
        fs::read(repository.root().join(".jj/pool.json")).expect("pool bytes"),
        before
    );
}

#[test]
fn grows_a_go_created_pool_without_replacing_its_membership_or_metadata() {
    let (_temporary_directory, repository) = go_repository_with_pool();
    let pool = repository.worker_pool();

    let growth = pool
        .resize_to(wsg_core::PoolCapacity::new(5).expect("capacity should be valid"))
        .expect("Go-created pool should grow");

    assert_eq!(growth.added_workers().len(), 1);
    let snapshot = pool.snapshot();
    let manifest = snapshot.pool().expect("pool manifest");
    assert_eq!(manifest.gh_repo(), "Jarvvski/jjfx");
    assert_eq!(manifest.size(), 5);
    assert_eq!(manifest.workers().len(), 5);
    assert_eq!(manifest.workers()[0].worker_id().as_str(), "worker-01");
    assert_eq!(manifest.workers()[3].worker_id().as_str(), "worker-04");
    assert_eq!(snapshot.workers().len(), 5);
    assert!(snapshot.diagnostics().is_empty());
}

#[test]
fn shrinks_a_go_created_pool_by_removing_a_terminal_tail_worker() {
    let (_temporary_directory, repository) = go_repository_with_pool();
    let pool = repository.worker_pool();

    let resized = pool
        .resize_to(wsg_core::PoolCapacity::new(3).expect("capacity"))
        .expect("failed Worker is terminal and removable");

    assert_eq!(resized.removed_workers()[0].as_str(), "worker-04");
    let snapshot = pool.snapshot();
    let manifest = snapshot.pool().expect("manifest");
    assert_eq!(manifest.size(), 3);
    assert_eq!(manifest.gh_repo(), "Jarvvski/jjfx");
    assert_eq!(manifest.workers()[2].worker_id().as_str(), "worker-03");
}

#[test]
fn repeated_resize_retries_detached_worker_cleanup() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    let grown = pool
        .resize_to(wsg_core::PoolCapacity::new(2).expect("capacity"))
        .expect("grow");
    let detached = grown.added_workers()[1].clone();
    let repo_state = repository.root().join(".jj/repo");
    let disabled = repository.root().join(".jj/repo-disabled");
    fs::rename(&repo_state, &disabled).expect("disable jj repository");

    let error = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect_err("Workspace cleanup should fail after membership commit");

    assert!(matches!(error, wsg_core::WorkerPoolError::Cleanup(_)));
    assert_eq!(pool.snapshot().pool().expect("manifest").size(), 1);
    let state = repository
        .root()
        .join(".jj/pool")
        .join(format!("{detached}.json"));
    let marker = repository
        .root()
        .join(".jj/pool")
        .join(format!("{detached}.cleanup"));
    assert!(
        state.exists(),
        "Worker state remains until cleanup succeeds"
    );
    assert!(
        marker.exists(),
        "membership removal leaves a cleanup marker"
    );
    fs::rename(&disabled, &repo_state).expect("restore jj repository");

    let retried = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect("same resize should retry detached cleanup");

    assert_eq!(retried.removed_workers(), &[detached]);
    assert!(!state.exists());
    assert!(!marker.exists());
}

#[test]
fn repeated_named_removal_retries_detached_worker_cleanup() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    let worker = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect("grow")
        .added_workers()[0]
        .clone();
    let repo_state = repository.root().join(".jj/repo");
    let disabled = repository.root().join(".jj/repo-disabled");
    fs::rename(&repo_state, &disabled).expect("disable jj repository");

    let error = pool
        .remove(worker.clone())
        .expect_err("Workspace cleanup should fail after membership commit");

    assert!(matches!(error, wsg_core::WorkerPoolError::Cleanup(_)));
    let marker = repository
        .root()
        .join(".jj/pool")
        .join(format!("{worker}.cleanup"));
    assert!(marker.exists());
    fs::rename(&disabled, &repo_state).expect("restore jj repository");

    let retried = pool
        .remove(worker.clone())
        .expect("same removal should retry detached cleanup");

    assert_eq!(retried.capacity().as_usize(), 0);
    assert_eq!(retried.removed_workers(), &[worker]);
    assert!(!marker.exists());
}

#[test]
fn no_op_resize_preserves_nonmember_state_without_a_cleanup_marker() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    pool.resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect("grow");
    let in_flight = wsg_core::WorkerId::parse("worker-in-flight").expect("Worker ID");
    let state = repository
        .root()
        .join(".jj/pool")
        .join(format!("{in_flight}.json"));
    fs::write(&state, fixture("worker-idle-claude.json")).expect("in-flight Worker state");

    let resized = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect("no-op resize");

    assert!(resized.removed_workers().is_empty());
    assert!(
        state.exists(),
        "unmarked in-flight state must not be cleaned"
    );
}

#[test]
fn named_removal_preserves_unknown_state_without_a_cleanup_marker() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    pool.resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect("grow");
    let unknown = wsg_core::WorkerId::parse("worker-unknown-state").expect("Worker ID");
    let state = repository
        .root()
        .join(".jj/pool")
        .join(format!("{unknown}.json"));
    fs::write(&state, fixture("worker-idle-claude.json")).expect("unknown Worker state");

    let error = pool
        .remove(unknown.clone())
        .expect_err("unmarked nonmember must remain unknown");

    assert!(matches!(
        error,
        wsg_core::WorkerPoolError::WorkerNotInPool { worker } if worker == unknown
    ));
    assert!(state.exists(), "unknown state must not be cleaned");
}

#[test]
fn busy_detached_cleanup_marker_is_not_torn_down() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    let worker = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect("grow")
        .added_workers()[0]
        .clone();
    let repo_state = repository.root().join(".jj/repo");
    let disabled = repository.root().join(".jj/repo-disabled");
    fs::rename(&repo_state, &disabled).expect("disable jj repository");
    pool.remove(worker.clone())
        .expect_err("cleanup should leave a durable marker");
    fs::rename(&disabled, &repo_state).expect("restore jj repository");
    let state = repository
        .root()
        .join(".jj/pool")
        .join(format!("{worker}.json"));
    let marker = repository
        .root()
        .join(".jj/pool")
        .join(format!("{worker}.cleanup"));
    fs::write(&state, fixture("worker-busy-claude.json")).expect("busy detached state");

    let error = pool
        .resize_to(wsg_core::PoolCapacity::new(0).expect("capacity"))
        .expect_err("busy detached Worker must not be torn down");

    assert!(matches!(
        error,
        wsg_core::WorkerPoolError::WorkersBusy { workers } if workers == vec![worker]
    ));
    assert!(state.exists());
    assert!(marker.exists());
}

#[test]
fn shrinking_permits_a_missing_tail_worker_state() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let grown = repository
        .worker_pool()
        .resize_to(wsg_core::PoolCapacity::new(2).expect("capacity"))
        .expect("grow");
    let missing = grown.added_workers()[1].clone();
    fs::remove_file(
        repository
            .root()
            .join(".jj/pool")
            .join(format!("{missing}.json")),
    )
    .expect("remove Worker state");

    let resized = repository
        .worker_pool()
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect("missing state should not imply busy");

    assert_eq!(resized.removed_workers(), &[missing]);
}

#[test]
fn worker_alias_is_trimmed_persisted_and_cosmetic() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    let worker = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect("grow")
        .added_workers()[0]
        .clone();
    let state_path = repository
        .root()
        .join(".jj/pool")
        .join(format!("{worker}.json"));
    let worker_state_before = fs::read(&state_path).expect("Worker state");
    let workspace_before = workspace_names(repository.root());

    pool.set_alias(worker.clone(), "  backend  ")
        .expect("set Worker alias");

    let reopened = Repository::open(repository.root()).expect("reopen repository");
    assert_eq!(
        reopened
            .worker_pool()
            .snapshot()
            .worker(worker.as_str())
            .expect("Worker snapshot")
            .alias(),
        "backend"
    );
    assert_eq!(
        fs::read(state_path).expect("Worker state"),
        worker_state_before
    );
    assert_eq!(workspace_names(repository.root()), workspace_before);
}

#[test]
fn blank_alias_clears_the_name_and_unknown_worker_does_not_mutate_pool() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    let worker = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect("grow")
        .added_workers()[0]
        .clone();
    pool.set_alias(worker.clone(), "backend")
        .expect("set Worker alias");

    pool.set_alias(worker.clone(), " \t ")
        .expect("clear Worker alias");

    assert_eq!(
        pool.snapshot()
            .worker(worker.as_str())
            .expect("Worker snapshot")
            .alias(),
        ""
    );
    let pool_path = repository.root().join(".jj/pool.json");
    let cleared: Value =
        serde_json::from_slice(&fs::read(&pool_path).expect("pool state")).expect("pool JSON");
    assert!(
        cleared.get("names").is_none(),
        "empty names should be omitted"
    );
    let before = fs::read(&pool_path).expect("pool bytes");
    let unknown = wsg_core::WorkerId::parse("worker-unknown").expect("Worker ID");

    let error = pool
        .set_alias(unknown.clone(), "backend")
        .expect_err("unknown Worker should be rejected");

    assert!(matches!(
        error,
        wsg_core::WorkerPoolError::WorkerNotInPool { worker } if worker == unknown
    ));
    assert_eq!(fs::read(pool_path).expect("pool bytes"), before);
}

#[test]
fn worker_alias_mutates_a_go_created_pool_without_losing_compatible_metadata() {
    let (temporary_directory, repository) = go_repository_with_pool();
    let pool_path = temporary_directory.path().join(".jj/pool.json");
    let mut state: Value =
        serde_json::from_slice(&fs::read(&pool_path).expect("pool state")).expect("pool JSON");
    state["foreground"] = Value::Bool(false);
    state["agent"] = Value::String("codex".to_owned());
    state["future"] = serde_json::json!({ "enabled": true });
    fs::write(
        &pool_path,
        serde_json::to_vec_pretty(&state).expect("pool JSON"),
    )
    .expect("pool state");
    let worker_state_path = temporary_directory.path().join(".jj/pool/worker-01.json");
    let worker_state_before = fs::read(&worker_state_path).expect("Worker state");
    let worker = wsg_core::WorkerId::parse("worker-01").expect("Worker ID");

    repository
        .worker_pool()
        .set_alias(worker.clone(), "primary")
        .expect("set alias on Go-created Pool");

    let snapshot = repository.worker_pool().snapshot();
    assert_eq!(
        snapshot
            .worker(worker.as_str())
            .expect("Worker snapshot")
            .alias(),
        "primary"
    );
    assert_eq!(snapshot.pool().expect("Pool snapshot").size(), 4);
    assert_eq!(
        snapshot
            .worker("worker-02")
            .expect("existing alias")
            .alias(),
        "beta",
        "unrelated aliases should survive"
    );
    assert_eq!(
        snapshot
            .pool()
            .expect("Pool snapshot")
            .workers()
            .iter()
            .map(|worker| worker.worker_id().as_str())
            .collect::<Vec<_>>(),
        ["worker-01", "worker-02", "worker-03", "worker-04"]
    );
    let written: Value =
        serde_json::from_slice(&fs::read(pool_path).expect("pool state")).expect("pool JSON");
    assert_eq!(written["future"], serde_json::json!({ "enabled": true }));
    assert_eq!(written["foreground"], false);
    assert_eq!(written["agent"], "codex");
    assert_eq!(
        fs::read(worker_state_path).expect("Worker state"),
        worker_state_before
    );
}

#[test]
fn alias_and_named_removal_serialize_without_reviving_removed_metadata() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    let worker = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect("grow")
        .added_workers()[0]
        .clone();
    let alias_pool = pool.clone();
    let remove_pool = pool.clone();
    let alias_worker = worker.clone();
    let remove_worker = worker.clone();

    let (alias, removal) = thread::scope(|scope| {
        let alias = scope.spawn(|| alias_pool.set_alias(alias_worker, "backend"));
        let removal = scope.spawn(|| remove_pool.remove(remove_worker));
        (
            alias.join().expect("alias thread"),
            removal.join().expect("remove thread"),
        )
    });

    assert!(removal.is_ok(), "idle Worker removal should succeed");
    assert!(
        alias.is_ok()
            || matches!(
                alias,
                Err(wsg_core::WorkerPoolError::WorkerNotInPool { .. })
            ),
        "alias must either serialize before removal or observe the removed Worker"
    );
    let snapshot = pool.snapshot();
    assert!(snapshot.worker(worker.as_str()).is_none());
    assert_eq!(snapshot.pool().expect("Pool snapshot").size(), 0);
}

#[test]
fn alias_and_shrink_serialize_without_retaining_a_detached_alias() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    let grown = pool
        .resize_to(wsg_core::PoolCapacity::new(2).expect("capacity"))
        .expect("grow");
    let worker = grown.added_workers()[1].clone();
    let alias_pool = pool.clone();
    let resize_pool = pool.clone();
    let alias_worker = worker.clone();

    let (alias, resize) = thread::scope(|scope| {
        let alias = scope.spawn(|| alias_pool.set_alias(alias_worker, "backend"));
        let resize = scope
            .spawn(|| resize_pool.resize_to(wsg_core::PoolCapacity::new(1).expect("capacity")));
        (
            alias.join().expect("alias thread"),
            resize.join().expect("resize thread"),
        )
    });

    assert!(resize.is_ok(), "idle tail shrink should succeed");
    assert!(
        alias.is_ok()
            || matches!(
                alias,
                Err(wsg_core::WorkerPoolError::WorkerNotInPool { .. })
            ),
        "alias must either serialize before shrink or observe the detached Worker"
    );
    let snapshot = pool.snapshot();
    assert!(snapshot.worker(worker.as_str()).is_none());
    assert_eq!(snapshot.pool().expect("Pool snapshot").size(), 1);
}

#[test]
fn alias_rejects_while_pool_destruction_is_in_progress() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    let worker = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect("grow")
        .added_workers()[0]
        .clone();
    let repo_state = repository.root().join(".jj/repo");
    let disabled = repository.root().join(".jj/repo-disabled");
    fs::rename(&repo_state, &disabled).expect("disable jj repository");
    pool.destroy()
        .expect_err("Workspace cleanup should leave destruction resumable");
    let before = fs::read(repository.root().join(".jj/pool.json")).expect("pool state");

    let error = pool
        .set_alias(worker, "backend")
        .expect_err("alias mutation should not race active destruction");

    assert!(matches!(error, wsg_core::WorkerPoolError::Conflict));
    assert_eq!(
        fs::read(repository.root().join(".jj/pool.json")).expect("pool state"),
        before
    );
    fs::rename(&disabled, &repo_state).expect("restore jj repository");
    pool.destroy().expect("resume destroy");
}

#[test]
fn named_removal_clears_a_go_created_worker_alias() {
    let (temporary_directory, repository) = go_repository_with_pool();
    fs::write(
        temporary_directory.path().join(".jj/pool/worker-02.json"),
        fixture("worker-idle-claude.json"),
    )
    .expect("make aliased Worker idle");
    let worker = wsg_core::WorkerId::parse("worker-02").expect("Worker ID");

    repository
        .worker_pool()
        .remove(worker.clone())
        .expect("remove aliased Worker");

    let loaded = repository.state_store().pool().load().expect("pool state");
    let wsg_core::Loaded::Present(pool) = loaded else {
        panic!("pool should remain");
    };
    assert!(!pool.value.names.contains_key(&worker));
    assert_eq!(pool.value.workers[1].as_str(), "worker-03");
}

#[test]
fn failed_pool_growth_leaves_no_registered_worker() {
    let (temporary_directory, repository) = local_repository_with_origin();
    fs::create_dir(temporary_directory.path().join(".env"))
        .expect("invalid environment source should be created");
    let pool = repository.worker_pool();

    let error = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity should be valid"))
        .expect_err("invalid setup source should fail growth");

    assert!(matches!(error, wsg_core::WorkerPoolError::Provision { .. }));
    let snapshot = pool.snapshot();
    assert_eq!(snapshot.pool().expect("empty pool manifest").size(), 0);
    assert!(snapshot.workers().is_empty());
    assert!(snapshot.diagnostics().is_empty());
}

#[test]
fn concurrent_growth_keeps_registered_workers_in_the_workspace_cache() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    pool.resize_to(wsg_core::PoolCapacity::new(1).expect("capacity should be valid"))
        .expect("initial pool should grow");

    let first_pool = pool.clone();
    let second_pool = pool.clone();
    let (first, second) = thread::scope(|scope| {
        let first = scope.spawn(|| {
            first_pool.resize_to(wsg_core::PoolCapacity::new(3).expect("capacity should be valid"))
        });
        let second = scope.spawn(|| {
            second_pool.resize_to(wsg_core::PoolCapacity::new(3).expect("capacity should be valid"))
        });
        (
            first.join().expect("first growth should not panic"),
            second.join().expect("second growth should not panic"),
        )
    });
    assert!(first.is_ok() || second.is_ok());

    let snapshot = pool.snapshot();
    let manifest = snapshot.pool().expect("pool manifest");
    assert_eq!(manifest.size(), 3);
    assert_eq!(snapshot.workers().len(), 3);
    assert!(snapshot.diagnostics().is_empty());
    let cache = fs::read_to_string(repository.root().join(".jj/ws-cache"))
        .expect("workspace cache should exist");
    for worker in manifest.workers() {
        assert!(
            cache.lines().any(|line| {
                line.split_once('\t')
                    .is_some_and(|(name, _)| name == worker.worker_id().as_str())
            }),
            "cache should contain {}",
            worker.worker_id()
        );
    }
}

#[test]
fn reservation_and_named_removal_never_claim_and_remove_the_same_worker() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    let worker = pool
        .resize_to(wsg_core::PoolCapacity::new(1).expect("capacity"))
        .expect("grow")
        .added_workers()[0]
        .clone();
    let reserve_pool = pool.clone();
    let remove_pool = pool.clone();
    let requested = worker.clone();

    let (reservation, removal) = thread::scope(|scope| {
        let reservation = scope.spawn(|| reserve_pool.reserve_named(requested, "AMBA-RACE"));
        let removal = scope.spawn(|| remove_pool.remove(worker.clone()));
        (
            reservation.join().expect("reservation thread"),
            removal.join().expect("removal thread"),
        )
    });

    assert_ne!(reservation.is_ok(), removal.is_ok());
    if reservation.is_ok() {
        assert!(pool.snapshot().worker(worker.as_str()).is_some());
    } else {
        assert!(pool.snapshot().worker(worker.as_str()).is_none());
    }
}

#[test]
fn reservation_and_shrink_never_claim_and_remove_the_same_worker() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    let grown = pool
        .resize_to(wsg_core::PoolCapacity::new(3).expect("capacity"))
        .expect("grow");
    for (worker, ticket) in grown.added_workers()[..2]
        .iter()
        .cloned()
        .zip(["AMBA-1", "AMBA-2"])
    {
        pool.reserve_named(worker, ticket).expect("reserve head");
    }
    let tail = grown.added_workers()[2].clone();
    let reserve_pool = pool.clone();
    let resize_pool = pool.clone();

    let (reservation, resize) = thread::scope(|scope| {
        let reservation = scope.spawn(|| reserve_pool.reserve("AMBA-RACE"));
        let resize = scope
            .spawn(|| resize_pool.resize_to(wsg_core::PoolCapacity::new(2).expect("capacity")));
        (
            reservation.join().expect("reservation thread"),
            resize.join().expect("resize thread"),
        )
    });

    if let Ok(reservation) = reservation {
        assert_eq!(reservation.worker_id(), &tail);
        assert!(resize.is_err());
        assert!(
            pool.snapshot()
                .pool()
                .expect("manifest")
                .workers()
                .iter()
                .any(|worker| worker.worker_id() == &tail)
        );
    } else {
        assert!(resize.is_ok());
        assert!(pool.snapshot().worker(tail.as_str()).is_none());
    }
}

#[test]
fn reservation_defaults_missing_pool_runtime_to_claude_and_persists_it() {
    let (temp, repository) = repository_with_pool();
    fs::write(
        temp.path().join(".jj/pool/worker-01.json"),
        fixture("worker-legacy-omits-runtime.json"),
    )
    .expect("make Worker omit its legacy runtime");

    let reservation = repository
        .worker_pool()
        .reserve("ENG-209")
        .expect("reservation should select the compatible default runtime");

    assert_eq!(reservation.agent_runtime(), AgentRuntime::Claude);
    let snapshot = repository.worker_pool().snapshot();
    assert_eq!(
        snapshot
            .worker("worker-01")
            .expect("reserved Worker")
            .agent_runtime(),
        Some(AgentRuntime::Claude)
    );
    let worker_json: Value = serde_json::from_slice(
        &fs::read(temp.path().join(".jj/pool/worker-01.json")).expect("persisted Worker"),
    )
    .expect("Worker JSON");
    assert_eq!(worker_json["agent"], "claude");
}

#[test]
fn reservation_normalizes_configured_codex_runtime_before_persisting_it() {
    let (temp, repository) = repository_with_pool();
    let pool_path = temp.path().join(".jj/pool.json");
    let mut pool: Value =
        serde_json::from_slice(&fs::read(&pool_path).expect("pool state")).expect("pool JSON");
    pool["agent"] = "  CODEX  ".into();
    fs::write(
        &pool_path,
        serde_json::to_vec(&pool).expect("pool JSON serialization"),
    )
    .expect("configured pool state");

    let reservation = repository
        .worker_pool()
        .reserve("ENG-210")
        .expect("configured Codex runtime should be accepted");

    assert_eq!(reservation.agent_runtime(), AgentRuntime::Codex);
    let worker_json: Value = serde_json::from_slice(
        &fs::read(temp.path().join(".jj/pool/worker-01.json")).expect("persisted Worker"),
    )
    .expect("Worker JSON");
    assert_eq!(worker_json["agent"], "codex");
    assert_eq!(
        repository
            .worker_pool()
            .snapshot()
            .worker("worker-01")
            .expect("Worker")
            .agent_runtime(),
        Some(AgentRuntime::Codex)
    );
}

#[test]
fn invalid_configured_runtime_fails_without_mutating_the_reserved_worker() {
    let (temp, repository) = repository_with_pool();
    let pool_path = temp.path().join(".jj/pool.json");
    let mut pool: Value =
        serde_json::from_slice(&fs::read(&pool_path).expect("pool state")).expect("pool JSON");
    pool["agent"] = "other".into();
    fs::write(
        &pool_path,
        serde_json::to_vec(&pool).expect("pool JSON serialization"),
    )
    .expect("configured pool state");
    let worker_path = temp.path().join(".jj/pool/worker-01.json");
    let before = fs::read(&worker_path).expect("Worker state");

    let error = repository
        .worker_pool()
        .reserve("ENG-211")
        .expect_err("unknown runtime should be rejected");

    assert!(matches!(
        error,
        wsg_core::WorkerPoolError::InvalidAgentRuntime { value } if value == "other"
    ));
    assert_eq!(fs::read(worker_path).expect("Worker state"), before);
}

#[test]
fn reservation_replaces_previous_runtime_while_preserving_worker_state() {
    let (temp, repository) = repository_with_pool();
    let pool_path = temp.path().join(".jj/pool.json");
    let mut pool: Value =
        serde_json::from_slice(&fs::read(&pool_path).expect("pool state")).expect("pool JSON");
    pool["agent"] = "codex".into();
    fs::write(
        &pool_path,
        serde_json::to_vec(&pool).expect("pool JSON serialization"),
    )
    .expect("configured pool state");
    let worker_path = temp.path().join(".jj/pool/worker-01.json");
    let mut worker: Value = serde_json::from_slice(&fs::read(&worker_path).expect("Worker state"))
        .expect("Worker JSON");
    worker["future"] = serde_json::json!({"enabled": true});
    fs::write(
        &worker_path,
        serde_json::to_vec(&worker).expect("Worker JSON serialization"),
    )
    .expect("Worker state");

    let reservation = repository
        .worker_pool()
        .reserve("ENG-212")
        .expect("Worker should be reservable");

    assert_eq!(reservation.agent_runtime(), AgentRuntime::Codex);
    let written: Value = serde_json::from_slice(&fs::read(&worker_path).expect("persisted Worker"))
        .expect("Worker JSON");
    assert_eq!(written["status"], "busy");
    assert_eq!(written["agent"], "codex");
    assert_eq!(written["ticket"], "ENG-212");
    assert_eq!(written["branch_name"], "eng-212");
    assert!(written["started_at"].is_string());
    assert!(written["log_file"].is_string());
    assert!(written["pid"].is_null());
    assert_eq!(written["future"]["enabled"], true);
}

#[test]
fn bulk_reservation_assigns_idle_workers_to_tickets_atomically_in_pool_order() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    let workers = pool
        .resize_to(wsg_core::PoolCapacity::new(3).expect("capacity"))
        .expect("grow Worker Pool")
        .added_workers()
        .to_vec();

    let reservations = pool
        .reserve_many(&["ENG-301", "ENG-302"])
        .expect("reserve complete batch");

    assert_eq!(reservations.len(), 2);
    assert_eq!(reservations[0].worker_id(), &workers[0]);
    assert_eq!(reservations[0].ticket(), "ENG-301");
    assert_eq!(reservations[1].worker_id(), &workers[1]);
    assert_eq!(reservations[1].ticket(), "ENG-302");
    let snapshot = pool.snapshot();
    assert_eq!(
        snapshot
            .worker(workers[0].as_str())
            .expect("first Worker")
            .ticket(),
        Some("ENG-301")
    );
    assert_eq!(
        snapshot
            .worker(workers[1].as_str())
            .expect("second Worker")
            .ticket(),
        Some("ENG-302")
    );
    assert_eq!(
        snapshot
            .worker(workers[2].as_str())
            .expect("unselected Worker")
            .status(),
        WorkerStatus::Idle
    );
}

#[test]
fn reserves_the_first_idle_worker_for_a_ticket() {
    let (_temp, repository) = repository_with_pool();
    let reservation = repository
        .worker_pool()
        .reserve("ENG-201")
        .expect("first idle Worker should be reserved");

    assert_eq!(reservation.worker_id().as_str(), "worker-01");
    assert_eq!(reservation.ticket(), "ENG-201");

    let snapshot = repository.worker_pool().snapshot();
    let worker = snapshot.worker("worker-01").expect("reserved Worker");
    assert_eq!(worker.status(), WorkerStatus::Busy);
    assert_eq!(worker.ticket(), Some("ENG-201"));
    assert_eq!(worker.branch_name(), Some("eng-201"));
    let expected_log = repository
        .root()
        .join(".jj/pool/worker-01.log")
        .to_string_lossy()
        .into_owned();
    assert_eq!(worker.log_file(), Some(expected_log.as_str()));
    assert!(worker.started_at().is_some());
}

#[test]
fn reserves_a_named_idle_worker_without_using_pool_order() {
    let (temp, repository) = repository_with_pool();
    fs::write(
        temp.path().join(".jj/pool/worker-04.json"),
        fixture("worker-idle-claude.json"),
    )
    .expect("make named Worker idle");
    let worker_id = wsg_core::WorkerId::parse("worker-04").expect("Worker ID");

    let reservation = repository
        .worker_pool()
        .reserve_named(worker_id, "ENG-202")
        .expect("named idle Worker should be reserved");

    assert_eq!(reservation.worker_id().as_str(), "worker-04");
    assert_eq!(reservation.ticket(), "ENG-202");
    let snapshot = repository.worker_pool().snapshot();
    assert_eq!(
        snapshot
            .worker("worker-04")
            .expect("reserved Worker")
            .status(),
        WorkerStatus::Busy
    );
    assert_eq!(
        snapshot.worker("worker-01").expect("first Worker").status(),
        WorkerStatus::Idle
    );
}

#[test]
fn named_reservation_rejects_unknown_or_busy_workers_without_mutation() {
    let (_temp, repository) = repository_with_pool();
    let unknown = wsg_core::WorkerId::parse("worker-nope").expect("Worker ID");
    let error = repository
        .worker_pool()
        .reserve_named(unknown, "ENG-203")
        .expect_err("unknown Worker should be rejected");
    assert!(matches!(
        error,
        wsg_core::WorkerPoolError::WorkerNotInPool { worker }
            if worker.as_str() == "worker-nope"
    ));

    let (_temp, repository) = repository_with_pool();
    let busy = wsg_core::WorkerId::parse("worker-02").expect("Worker ID");
    let error = repository
        .worker_pool()
        .reserve_named(busy, "ENG-204")
        .expect_err("busy Worker should be rejected");
    assert!(matches!(
        error,
        wsg_core::WorkerPoolError::WorkerNotIdle { worker }
            if worker.as_str() == "worker-02"
    ));
    let snapshot = repository.worker_pool().snapshot();
    let worker = snapshot.worker("worker-02").expect("busy Worker");
    assert_eq!(worker.ticket(), Some("ENG-101"));
}

#[test]
fn concurrent_reservations_allocate_distinct_idle_workers() {
    let (temp, repository) = repository_with_pool();
    fs::write(
        temp.path().join(".jj/pool/worker-04.json"),
        fixture("worker-idle-claude.json"),
    )
    .expect("make second Worker idle");
    let first_pool = repository.worker_pool();
    let second_pool = first_pool.clone();

    let (first, second) = thread::scope(|scope| {
        let first = scope.spawn(|| first_pool.reserve("ENG-205"));
        let second = scope.spawn(|| second_pool.reserve("ENG-206"));
        (
            first.join().expect("first reservation should not panic"),
            second.join().expect("second reservation should not panic"),
        )
    });
    let first = first.expect("first reservation should succeed");
    let second = second.expect("second reservation should succeed");
    assert_ne!(first.worker_id(), second.worker_id());
    assert_eq!(first.ticket(), "ENG-205");
    assert_eq!(second.ticket(), "ENG-206");
}

#[test]
fn reservation_reports_no_idle_capacity_without_mutation() {
    let (temp, repository) = repository_with_pool();
    fs::write(
        temp.path().join(".jj/pool/worker-01.json"),
        fixture("worker-busy-claude.json"),
    )
    .expect("make every Worker non-idle");

    let error = repository
        .worker_pool()
        .reserve("ENG-207")
        .expect_err("a full pool should reject the Reservation");
    assert!(matches!(
        error,
        wsg_core::WorkerPoolError::NoIdleWorkers {
            ticket,
            available: 0
        } if ticket == "ENG-207"
    ));
    let snapshot = repository.worker_pool().snapshot();
    assert_eq!(
        snapshot.worker("worker-01").expect("Worker").ticket(),
        Some("ENG-101")
    );
}

#[test]
fn reservation_preserves_go_worker_extensions_and_null_fields() {
    let (temp, repository) = repository_with_pool();
    let worker_path = temp.path().join(".jj/pool/worker-01.json");
    let mut worker: Value =
        serde_json::from_slice(&fs::read(&worker_path).expect("Go Worker state"))
            .expect("Worker JSON");
    worker["future"] = serde_json::json!({"enabled": true});
    fs::write(
        &worker_path,
        serde_json::to_vec(&worker).expect("Worker JSON"),
    )
    .expect("Worker state");

    repository
        .worker_pool()
        .reserve("ENG-208")
        .expect("Go-created Worker should be reservable");

    let written: Value =
        serde_json::from_slice(&fs::read(&worker_path).expect("reserved Worker state"))
            .expect("reserved Worker JSON");
    assert_eq!(written["future"]["enabled"], true);
    assert_eq!(written["agent"], "claude");
    for field in ["completed_at", "error", "exit_code", "pid"] {
        assert!(
            written[field].is_null(),
            "{field} should remain explicit null"
        );
    }
}

#[test]
fn reconcile_runs_returns_snapshot_without_mutating_worker_without_pid() {
    let (temp, repository) = repository_with_pool();
    let worker_path = temp.path().join(".jj/pool/worker-02.json");
    let mut worker: Value =
        serde_json::from_slice(&fs::read(&worker_path).expect("busy Worker state"))
            .expect("Worker JSON");
    worker["pid"] = Value::Null;
    fs::write(
        &worker_path,
        serde_json::to_vec(&worker).expect("Worker JSON"),
    )
    .expect("Worker state");

    let snapshot = repository.worker_pool().reconcile_runs();

    let worker = snapshot.worker("worker-02").expect("busy Worker");
    assert_eq!(worker.status(), WorkerStatus::Busy);
    assert_eq!(worker.pid(), None);
    assert!(snapshot.diagnostics().is_empty());
}

#[test]
fn reconcile_runs_finalizes_busy_worker_with_dead_pid_once() {
    let (temp, repository) = repository_with_pool();
    let worker_path = temp.path().join(".jj/pool/worker-02.json");
    let mut exited = Command::new("sh")
        .args(["-c", "exit 0"])
        .spawn()
        .expect("dead PID helper");
    let dead_pid = exited.id();
    exited.wait().expect("dead PID helper should exit");

    let mut worker: Value =
        serde_json::from_slice(&fs::read(&worker_path).expect("busy Worker state"))
            .expect("Worker JSON");
    worker["pid"] = serde_json::json!(dead_pid);
    fs::write(
        &worker_path,
        serde_json::to_vec(&worker).expect("Worker JSON"),
    )
    .expect("Worker state");

    let first = repository.worker_pool().reconcile_runs();

    let worker = first.worker("worker-02").expect("reconciled Worker");
    assert_eq!(worker.status(), WorkerStatus::Failed);
    assert_eq!(worker.exit_code(), Some(1));
    assert_eq!(worker.error(), Some("Process exited unexpectedly"));
    assert!(worker.completed_at().is_some());
    assert_eq!(worker.pid(), Some(dead_pid));

    let written = fs::read(&worker_path).expect("reconciled Worker state");
    let second = repository.worker_pool().reconcile_runs();
    assert_eq!(second.worker("worker-02"), first.worker("worker-02"));
    assert_eq!(
        fs::read(&worker_path).expect("reconciled Worker state"),
        written
    );
}

#[test]
fn reconcile_runs_continues_past_missing_and_malformed_workers() {
    let (temp, repository) = repository_with_pool();
    let pool = temp.path().join(".jj/pool");
    fs::remove_file(pool.join("worker-03.json")).expect("remove Worker");
    fs::write(pool.join("worker-04.json"), b"{ not json").expect("malformed Worker");
    let worker_path = pool.join("worker-02.json");
    let mut worker: Value =
        serde_json::from_slice(&fs::read(&worker_path).expect("busy Worker state"))
            .expect("Worker JSON");
    worker["pid"] = Value::Null;
    fs::write(
        &worker_path,
        serde_json::to_vec(&worker).expect("Worker JSON"),
    )
    .expect("Worker state");

    let snapshot = repository.worker_pool().reconcile_runs();

    assert!(snapshot.worker("worker-01").is_some());
    assert!(snapshot.worker("worker-02").is_some());
    assert!(snapshot.worker("worker-03").is_none());
    assert!(snapshot.worker("worker-04").is_none());
    assert!(snapshot.diagnostics().iter().any(|diagnostic| {
        diagnostic.worker_id().map(|id| id.as_str()) == Some("worker-03")
            && diagnostic.kind() == SnapshotDiagnosticKind::MissingWorker
    }));
    assert!(snapshot.diagnostics().iter().any(|diagnostic| {
        diagnostic.worker_id().map(|id| id.as_str()) == Some("worker-04")
            && diagnostic.kind() == SnapshotDiagnosticKind::MalformedWorker
    }));
}

#[test]
fn reads_go_pool_workers_as_an_immutable_snapshot() {
    let (_temp, repository) = repository_with_pool();
    let snapshot = repository.read_worker_pool_snapshot();
    assert_eq!(snapshot.pool().expect("pool").size(), 4);
    assert_eq!(snapshot.workers().len(), 4);
    let busy = snapshot.worker("worker-02").expect("busy Worker");
    assert_eq!(busy.alias(), "beta");
    assert_eq!(busy.workspace(), "worker-02");
    assert_eq!(busy.status(), WorkerStatus::Busy);
    assert_eq!(busy.agent_runtime(), Some(AgentRuntime::Claude));
    assert_eq!(busy.ticket(), Some("ENG-101"));
    assert_eq!(busy.started_at(), Some("2026-07-27T10:01:00Z"));
}

#[test]
fn missing_and_malformed_worker_state_do_not_hide_healthy_workers() {
    let (_temp, repository) = repository_with_pool();
    let pool = repository.root().join(".jj/pool");
    fs::remove_file(pool.join("worker-04.json")).expect("remove Worker");
    fs::write(pool.join("worker-03.json"), b"{ not json").expect("malformed Worker");
    let snapshot = repository.read_worker_pool_snapshot();
    assert_eq!(snapshot.workers().len(), 2);
    assert!(snapshot.diagnostics().iter().any(|diagnostic| {
        diagnostic.worker_id().map(|id| id.as_str()) == Some("worker-03")
            && diagnostic.kind() == SnapshotDiagnosticKind::MalformedWorker
    }));
    assert!(snapshot.diagnostics().iter().any(|diagnostic| {
        diagnostic.worker_id().map(|id| id.as_str()) == Some("worker-04")
            && diagnostic.kind() == SnapshotDiagnosticKind::MissingWorker
    }));
}

#[test]
fn missing_pool_is_not_invented_as_empty() {
    let temp = tempfile::tempdir().expect("temp repository");
    fs::create_dir(temp.path().join(".jj")).expect("repository marker");
    let repository = Repository::open(temp.path()).expect("repository");
    let snapshot = repository.read_worker_pool_snapshot();
    assert!(snapshot.pool().is_none());
    assert!(snapshot.is_missing());
}

#[test]
fn snapshot_read_changes_no_file_and_creates_no_lock() {
    let (_temp, repository) = repository_with_pool();
    let pool_file = repository.root().join(".jj/pool.json");
    let before = fs::read(&pool_file).expect("pool bytes");
    let _ = repository.read_worker_pool_snapshot();
    assert_eq!(fs::read(pool_file).expect("pool bytes"), before);
    assert!(!repository.root().join(".jj/pool/.dispatch.lock").exists());
    assert!(
        !repository
            .root()
            .join(".jj/pool/worker-01.json.lock")
            .exists()
    );
}

#[test]
fn worker_without_agent_remains_readable() {
    let (temp, repository) = repository_with_pool();
    fs::write(
        temp.path().join(".jj/pool/worker-01.json"),
        fixture("worker-legacy-omits-runtime.json"),
    )
    .expect("legacy Worker");
    let worker = repository
        .read_worker_pool_snapshot()
        .worker("worker-01")
        .cloned()
        .expect("legacy Worker");
    assert_eq!(worker.agent_runtime(), None);
}
