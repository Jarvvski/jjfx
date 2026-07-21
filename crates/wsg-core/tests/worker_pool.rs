use std::fs;
use std::path::Path;

use tempfile::TempDir;
use wsg_core::{AgentRuntime, PersistedField, Repository, SnapshotDiagnosticKind, WorkerStatus};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/compatibility");

fn fixture(name: &str) -> Vec<u8> {
    fs::read(Path::new(FIXTURES).join(name)).expect("fixture should be readable")
}

fn repository_with_pool() -> (TempDir, Repository) {
    let temp = tempfile::tempdir().expect("temporary repository should be created");
    let jj = temp.path().join(".jj");
    let workers = jj.join("workers");
    fs::create_dir_all(&workers).expect("worker state directory should be created");
    fs::write(jj.join("pool.json"), fixture("pool-workers.json"))
        .expect("pool state should be written");
    for (worker_id, fixture_name) in [
        ("worker-01", "worker-idle-claude.json"),
        ("worker-02", "worker-busy-claude.json"),
        ("worker-03", "worker-done-codex.json"),
        ("worker-04", "worker-failed-codex.json"),
    ] {
        fs::write(
            workers.join(format!("{worker_id}.json")),
            fixture(fixture_name),
        )
        .expect("worker state should be written");
    }
    let repository = Repository::open(temp.path()).expect("temporary repository should open");
    (temp, repository)
}

#[test]
fn reads_pool_workers_as_an_immutable_snapshot() {
    let (_temp, repository) = repository_with_pool();

    let snapshot = repository.read_worker_pool_snapshot();

    assert_eq!(
        snapshot.pool().expect("pool should be present").version(),
        1
    );
    assert_eq!(snapshot.workers().len(), 4);
    let busy = snapshot
        .worker("worker-02")
        .expect("busy worker should be present");
    assert_eq!(busy.alias(), "beta");
    assert_eq!(busy.workspace(), "worker-beta");
    assert_eq!(busy.status(), WorkerStatus::Busy);
    assert_eq!(busy.agent_runtime(), Some(AgentRuntime::Claude));
    assert_eq!(busy.ticket(), Some("ENG-101"));
    assert_eq!(busy.run(), Some("run-20260721-01"));
    assert_eq!(busy.started_at(), Some("2026-07-21T10:00:00Z"));
}

#[test]
fn missing_and_malformed_worker_state_do_not_hide_healthy_workers() {
    let (_temp, repository) = repository_with_pool();
    let workers = repository.root().join(".jj/workers");
    fs::remove_file(workers.join("worker-04.json")).expect("worker should be removed");
    fs::write(workers.join("worker-03.json"), b"{ not json")
        .expect("malformed worker should be written");

    let snapshot = repository.read_worker_pool_snapshot();

    assert_eq!(snapshot.workers().len(), 2);
    assert!(snapshot.worker("worker-01").is_some());
    assert!(snapshot.worker("worker-02").is_some());
    assert!(snapshot.worker("worker-03").is_none());
    assert!(snapshot.worker("worker-04").is_none());
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
fn a_missing_pool_is_reported_without_inventing_an_empty_pool() {
    let temp = tempfile::tempdir().expect("temporary repository should be created");
    fs::create_dir(temp.path().join(".jj")).expect("repository marker should be created");
    let repository = Repository::open(temp.path()).expect("temporary repository should open");

    let snapshot = repository.read_worker_pool_snapshot();

    assert!(snapshot.pool().is_none());
    assert!(snapshot.workers().is_empty());
    assert!(
        snapshot
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.kind() == SnapshotDiagnosticKind::MissingPool)
    );
}

#[test]
fn reading_a_snapshot_does_not_change_state_files() {
    let (_temp, repository) = repository_with_pool();
    let paths = [
        repository.root().join(".jj/pool.json"),
        repository.root().join(".jj/workers/worker-01.json"),
        repository.root().join(".jj/workers/worker-02.json"),
        repository.root().join(".jj/workers/worker-03.json"),
        repository.root().join(".jj/workers/worker-04.json"),
    ];
    let before: Vec<_> = paths
        .iter()
        .map(|path| {
            (
                fs::read(path).expect("state should be readable"),
                fs::metadata(path)
                    .expect("state metadata should be readable")
                    .modified()
                    .expect("state mtime should be readable"),
            )
        })
        .collect();

    let _snapshot = repository.read_worker_pool_snapshot();

    for (path, (bytes, modified)) in paths.iter().zip(before) {
        let metadata = fs::metadata(path).expect("state metadata should remain readable");
        assert_eq!(fs::read(path).expect("state should remain readable"), bytes);
        assert_eq!(
            metadata
                .modified()
                .expect("state mtime should remain readable"),
            modified
        );
    }
}

#[test]
fn legacy_worker_without_runtime_remains_readable() {
    let (temp, repository) = repository_with_pool();
    fs::write(
        temp.path().join(".jj/pool.json"),
        br#"{"version":1,"workers":[{"worker_id":"worker-legacy","workspace":"worker-legacy"}]}"#,
    )
    .expect("pool state should be replaced");
    fs::write(
        temp.path().join(".jj/workers/worker-legacy.json"),
        fixture("worker-legacy-omits-runtime.json"),
    )
    .expect("legacy worker should be written");

    let snapshot = repository.read_worker_pool_snapshot();
    let worker = snapshot
        .worker("worker-legacy")
        .expect("legacy worker should be present");
    assert_eq!(worker.agent_runtime(), None);
    assert_eq!(worker.agent_runtime_presence(), PersistedField::Missing);

    let (_temp, repository) = repository_with_pool();
    let idle_snapshot = repository.read_worker_pool_snapshot();
    let idle = idle_snapshot
        .worker("worker-01")
        .expect("idle worker should be present");
    assert_eq!(idle.ticket_presence(), PersistedField::Null);
}
