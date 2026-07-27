use std::fs;
use std::path::Path;

use tempfile::TempDir;
use wsg_core::{AgentRuntime, Repository, SnapshotDiagnosticKind, WorkerStatus};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/compatibility");
fn fixture(name: &str) -> Vec<u8> {
    fs::read(Path::new(FIXTURES).join(name)).expect("fixture")
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
