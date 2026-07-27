use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;

use tempfile::TempDir;
use wsg_core::{AgentRuntime, Repository, SnapshotDiagnosticKind, WorkerStatus};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/compatibility");
fn fixture(name: &str) -> Vec<u8> {
    fs::read(Path::new(FIXTURES).join(name)).expect("fixture")
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
fn creates_a_pool_with_one_worker_visible_through_its_snapshot() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();

    let growth = pool
        .grow_to(wsg_core::PoolCapacity::new(1).expect("capacity should be valid"))
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
        .grow_to(wsg_core::PoolCapacity::new(1).expect("capacity should be valid"))
        .expect("initial pool should grow");
    let existing = first.added_workers()[0].clone();

    let second = pool
        .grow_to(wsg_core::PoolCapacity::new(3).expect("capacity should be valid"))
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
        .grow_to(wsg_core::PoolCapacity::new(1).expect("capacity should be valid"))
        .expect("initial pool should grow");
    let before = fs::read(repository.root().join(".jj/pool.json")).expect("pool bytes");

    let second = pool
        .grow_to(wsg_core::PoolCapacity::new(1).expect("capacity should be valid"))
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
fn growing_to_a_lower_capacity_rejects_without_mutation() {
    let (_temporary_directory, repository) = local_repository_with_origin();
    let pool = repository.worker_pool();
    pool.grow_to(wsg_core::PoolCapacity::new(2).expect("capacity should be valid"))
        .expect("initial pool should grow");
    let before = fs::read(repository.root().join(".jj/pool.json")).expect("pool bytes");

    let error = pool
        .grow_to(wsg_core::PoolCapacity::new(1).expect("capacity should be valid"))
        .expect_err("shrinking belongs to a later lifecycle operation");

    assert!(matches!(
        error,
        wsg_core::WorkerPoolError::CannotShrink {
            current: 2,
            requested: 1
        }
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
        .grow_to(wsg_core::PoolCapacity::new(5).expect("capacity should be valid"))
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
fn failed_pool_growth_leaves_no_registered_worker() {
    let (temporary_directory, repository) = local_repository_with_origin();
    fs::create_dir(temporary_directory.path().join(".env"))
        .expect("invalid environment source should be created");
    let pool = repository.worker_pool();

    let error = pool
        .grow_to(wsg_core::PoolCapacity::new(1).expect("capacity should be valid"))
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
    pool.grow_to(wsg_core::PoolCapacity::new(1).expect("capacity should be valid"))
        .expect("initial pool should grow");

    let first_pool = pool.clone();
    let second_pool = pool.clone();
    let (first, second) = thread::scope(|scope| {
        let first = scope.spawn(|| {
            first_pool.grow_to(wsg_core::PoolCapacity::new(3).expect("capacity should be valid"))
        });
        let second = scope.spawn(|| {
            second_pool.grow_to(wsg_core::PoolCapacity::new(3).expect("capacity should be valid"))
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
    assert!(!repository
        .root()
        .join(".jj/pool/worker-01.json.lock")
        .exists());
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
