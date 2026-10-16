use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use jjfx::engine::{
    CommitOutcome, DEFAULT_POOL_NAME, DispatchGroupState, Expected, Loaded, PoolCapacity, PoolName,
    PoolState, Repository, RepositoryError, StateChange, TicketId, WireStatus, WireTimestamp,
    WorkerId, WorkerState,
};
use serde_json::Value;
use tempfile::TempDir;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/state");
fn fixture(name: &str) -> Vec<u8> {
    fs::read(Path::new(FIXTURES).join(name)).expect("fixture")
}
fn repository() -> (TempDir, Repository) {
    let temp = tempfile::tempdir().expect("temp repository");
    fs::create_dir(temp.path().join(".jj")).expect("repository marker");
    let repository = Repository::open(temp.path()).expect("repository");
    (temp, repository)
}

/// Builds a Repository in the single-pool layout jjfx used before pools were
/// named: the document `.jj/pool.json` beside the directory `.jj/pool/`, with
/// the repository-scoped documents mixed in among the pool's.
fn legacy_repository() -> (TempDir, PathBuf) {
    let temp = tempfile::tempdir().expect("temp repository");
    let root = temp.path().to_path_buf();
    fs::create_dir(root.join(".jj")).expect("repository marker");
    let pool = root.join(".jj/pool");
    fs::create_dir_all(&pool).expect("legacy pool directory");
    fs::write(root.join(".jj/pool.json"), fixture("pool-empty.json")).expect("legacy document");
    fs::write(
        pool.join("worker-01.json"),
        fixture("worker-idle-claude.json"),
    )
    .expect("legacy Worker");
    fs::write(pool.join("waiting.json"), br#"{"tickets":["ENG-1"]}"#).expect("legacy waiting set");
    fs::write(
        pool.join("sessions.json"),
        br#"{"sessions":{"worker-01":"ses_one"}}"#,
    )
    .expect("legacy sessions");
    fs::write(pool.join("runs.jsonl"), b"").expect("legacy Run records");
    fs::write(pool.join(".dispatch.lock"), b"").expect("legacy pool lock");
    fs::write(pool.join("in-use.json"), br#"{"workers":["worker-01"]}"#).expect("legacy occupancy");
    fs::write(pool.join("undo.json"), br#"{"mutations":{}}"#).expect("legacy undo");
    (temp, root)
}

#[test]
fn pool_repository_creates_replaces_conflicts_and_removes() {
    let (_temp, repository) = repository();
    let pool = repository.state_store().pool();
    assert!(matches!(pool.load().expect("load"), Loaded::Missing));
    let state: PoolState =
        serde_json::from_slice(&fixture("pool-empty.json")).expect("legacy pool");
    let applied = pool
        .commit(Expected::Missing, StateChange::Replace(state))
        .expect("create");
    let loaded = match applied {
        CommitOutcome::Applied(loaded) => loaded,
        CommitOutcome::Conflict(_) => panic!("unexpected conflict"),
    };
    let Loaded::Present(first) = loaded else {
        panic!("missing after create")
    };
    let stale = first.revision().clone();
    let (_, revision) = first.into_parts();
    let mut replacement = match pool.load().expect("reload") {
        Loaded::Present(value) => value.value,
        Loaded::Missing => panic!("missing"),
    };
    replacement.gh_repo = "changed/repo".to_owned();
    assert!(matches!(
        pool.commit(Expected::Match(revision), StateChange::Replace(replacement))
            .expect("replace"),
        CommitOutcome::Applied(_)
    ));
    assert!(matches!(
        pool.commit(Expected::Match(stale), StateChange::Remove)
            .expect("stale remove"),
        CommitOutcome::Conflict(_)
    ));
    let revision = match pool.load().expect("reload") {
        Loaded::Present(value) => value.revision().clone(),
        Loaded::Missing => panic!("missing"),
    };
    assert!(matches!(
        pool.commit(Expected::Match(revision), StateChange::Remove)
            .expect("remove"),
        CommitOutcome::Applied(Loaded::Missing)
    ));
}

#[test]
fn worker_repository_round_trips_go_state_and_unknown_fields() {
    let (temp, repository) = repository();
    let worker = WorkerId::parse("worker-01").expect("Worker ID");
    let path = temp.path().join(".jj/pools/default/worker-01.json");
    fs::create_dir_all(path.parent().expect("parent")).expect("pool directory");
    let mut source: Value =
        serde_json::from_slice(&fixture("worker-busy-claude.json")).expect("legacy Worker");
    source["future"] = serde_json::json!({"enabled": true});
    fs::write(&path, serde_json::to_vec(&source).expect("json")).expect("Worker state");
    let repository = repository.state_store().worker(worker);
    let versioned = match repository.load().expect("load") {
        Loaded::Present(value) => value,
        Loaded::Missing => panic!("missing"),
    };
    let (mut state, revision) = versioned.into_parts();
    state.ticket = Some("ENG-200".to_owned());
    repository
        .commit(Expected::Match(revision), StateChange::Replace(state))
        .expect("commit");
    let written: Value =
        serde_json::from_slice(&fs::read(path).expect("written Worker")).expect("json");
    assert_eq!(written["ticket"], "ENG-200");
    assert_eq!(written["future"]["enabled"], true);
}

#[test]
fn dispatch_group_repository_uses_lowercase_compatible_name() {
    let (temp, repository) = repository();
    let parent = TicketId::parse("ENG-100").expect("Ticket ID");
    let state: DispatchGroupState =
        serde_json::from_slice(&fixture("dispatch-pending.json")).expect("Go group");
    let group = repository.state_store().dispatch_group(parent);
    assert!(matches!(
        group
            .commit(Expected::Missing, StateChange::Replace(state))
            .expect("create"),
        CommitOutcome::Applied(_)
    ));
    assert!(
        temp.path()
            .join(".jj/pools/default/dispatch-eng-100.json")
            .exists()
    );
    assert!(
        temp.path()
            .join(".jj/pools/default/.dispatch.lock")
            .exists()
    );
    assert!(
        temp.path()
            .join(".jj/pools/default/dispatch-eng-100.json.lock")
            .exists()
    );
}

#[test]
fn invalid_replacement_preserves_previous_valid_file() {
    let (temp, repository) = repository();
    let pool = repository.state_store().pool();
    let state: PoolState =
        serde_json::from_slice(&fixture("pool-empty.json")).expect("legacy pool");
    pool.commit(Expected::Missing, StateChange::Replace(state))
        .expect("create");
    let path = temp.path().join(".jj/pools/default/pool.json");
    let before = fs::read(&path).expect("previous state");
    let versioned = match pool.load().expect("load") {
        Loaded::Present(value) => value,
        Loaded::Missing => panic!("missing"),
    };
    let (mut invalid, revision) = versioned.into_parts();
    invalid.size = 1;
    assert!(
        pool.commit(Expected::Match(revision), StateChange::Replace(invalid))
            .is_err()
    );
    assert_eq!(fs::read(path).expect("preserved state"), before);
}

#[test]
fn malformed_state_is_an_error_not_missing() {
    let (temp, repository) = repository();
    let path = temp.path().join(".jj/pools/default/pool.json");
    fs::create_dir_all(path.parent().expect("parent")).expect("pool directory");
    fs::write(&path, b"{ malformed").expect("malformed pool");
    assert!(repository.state_store().pool().load().is_err());
}

#[test]
fn persisted_identifiers_are_validated_before_path_construction() {
    let (temp, repository) = repository();
    let path = temp.path().join(".jj/pools/default/pool.json");
    fs::create_dir_all(path.parent().expect("parent")).expect("pool directory");
    fs::write(
        &path,
        br#"{"size":1,"gh_repo":"Jarvvski/jjfx","workers":["../escape"],"created_at":"2026-07-27T10:00:00Z"}"#,
    )
    .expect("invalid pool");
    assert!(repository.state_store().pool().load().is_err());
}

#[test]
fn dispatch_group_body_must_match_repository_parent() {
    let (temp, repository) = repository();
    let parent = TicketId::parse("ENG-100").expect("Ticket ID");
    let mut state: DispatchGroupState =
        serde_json::from_slice(&fixture("dispatch-pending.json")).expect("Go group");
    state.parent = TicketId::parse("ENG-OTHER").expect("other Ticket ID");
    let group = repository.state_store().dispatch_group(parent);
    assert!(
        group
            .commit(Expected::Missing, StateChange::Replace(state))
            .is_err()
    );
    assert!(
        !temp
            .path()
            .join(".jj/pools/default/dispatch-eng-100.json")
            .exists()
    );
}

#[test]
fn identifiers_reject_paths() {
    assert!(WorkerId::parse("../worker").is_err());
    assert!(WorkerId::parse("").is_err());
    assert!(TicketId::parse("ENG/100").is_err());
}

#[test]
fn public_state_models_accept_forward_compatible_wire_values() {
    let worker: WorkerState = serde_json::from_slice(br#"{"status":"future","agent":"new-runtime","ticket":null,"pid":null,"started_at":null,"completed_at":null,"log_file":null,"branch_name":null,"exit_code":null,"error":null}"#).expect("forward-compatible Worker");
    assert_eq!(worker.status.as_str(), "future");
    assert_eq!(worker.agent.expect("agent").as_str(), "new-runtime");
}

#[test]
fn go_omitempty_and_branch_null_semantics_are_preserved() {
    let (temp, repository) = repository();
    let mut pool: PoolState =
        serde_json::from_slice(&fixture("pool-empty.json")).expect("legacy pool");
    pool.agent = Some(jjfx::engine::WireAgent::new(""));
    repository
        .state_store()
        .pool()
        .commit(Expected::Missing, StateChange::Replace(pool))
        .expect("pool commit");
    let pool_json: Value = serde_json::from_slice(
        &fs::read(temp.path().join(".jj/pools/default/pool.json")).expect("pool state"),
    )
    .expect("pool JSON");
    assert!(pool_json.get("agent").is_none());

    let worker_id = WorkerId::parse("worker-01").expect("Worker ID");
    let mut worker = WorkerState::new(jjfx::engine::WireStatus::new("idle"));
    worker.branch_name = Some(String::new());
    repository
        .state_store()
        .worker(worker_id)
        .commit(Expected::Missing, StateChange::Replace(worker))
        .expect("Worker commit");
    let worker_json: Value = serde_json::from_slice(
        &fs::read(temp.path().join(".jj/pools/default/worker-01.json")).expect("Worker state"),
    )
    .expect("Worker JSON");
    assert!(worker_json["branch_name"].is_null());
}

#[test]
fn unknown_json_number_spelling_survives_a_worker_mutation() {
    let (temp, repository) = repository();
    let path = temp.path().join(".jj/pools/default/worker-01.json");
    fs::create_dir_all(path.parent().expect("parent")).expect("pool directory");
    fs::write(
        &path,
        br#"{"status":"idle","agent":null,"ticket":null,"pid":null,"started_at":null,"completed_at":null,"log_file":null,"branch_name":null,"exit_code":null,"error":null,"future_number":1.2300e+100}"#,
    )
    .expect("Worker state");
    let worker = repository
        .state_store()
        .worker(WorkerId::parse("worker-01").expect("Worker ID"));
    let versioned = match worker.load().expect("load") {
        Loaded::Present(value) => value,
        Loaded::Missing => panic!("missing"),
    };
    let (mut state, revision) = versioned.into_parts();
    state.ticket = Some("ENG-200".to_owned());
    worker
        .commit(Expected::Match(revision), StateChange::Replace(state))
        .expect("commit");
    let written = fs::read_to_string(path).expect("written Worker");
    assert!(written.contains("1.2300e+100"), "written state: {written}");
}

#[test]
fn a_failed_pool_shrink_leaves_no_cleanup_marker_on_a_member() {
    let (temp, repository) = repository();
    let first = WorkerId::parse("worker-01").expect("Worker");
    let second = WorkerId::parse("worker-02").expect("Worker");
    repository
        .state_store()
        .pool()
        .commit(
            Expected::Missing,
            StateChange::Replace(PoolState::new(
                2,
                "owner/repo",
                vec![first.clone(), second.clone()],
                WireTimestamp::new("2026-08-04T12:00:00Z"),
            )),
        )
        .expect("create pool");
    for worker in [&first, &second] {
        repository
            .state_store()
            .worker(worker.clone())
            .commit(
                Expected::Missing,
                StateChange::Replace(WorkerState::new(WireStatus::new("idle"))),
            )
            .expect("idle Worker");
    }

    // Make the pool directory read-only so the Worker Pool manifest's atomic write
    // cannot create its temporary file. The cleanup markers live in the same
    // directory as the manifest now, so this asserts the failure path leaves no
    // marker behind: a member carrying one would fail `persist_reserved_pid`
    // forever, and no Run could start on it.
    let pool_directory = temp.path().join(".jj/pools/default");
    let original = fs::metadata(&pool_directory)
        .expect("metadata")
        .permissions();
    let mut read_only = original.clone();
    read_only.set_mode(0o555);
    fs::set_permissions(&pool_directory, read_only).expect("make pool directory read-only");

    let result = repository
        .worker_pool()
        .resize_to(PoolCapacity::new(1).expect("capacity"));

    fs::set_permissions(&pool_directory, original).expect("restore pool directory permissions");

    assert!(result.is_err(), "the failed manifest write must surface");
    let marker = temp.path().join(".jj/pools/default/worker-02.cleanup");
    assert!(
        !marker.exists(),
        "a Pool member must never carry a cleanup marker after a failed shrink"
    );
}

#[test]
fn session_sidecar_records_reads_and_replaces_worker_sessions() {
    let (temp, repository) = repository();
    let sessions = repository.state_store().sessions();
    let worker = WorkerId::parse("worker-01").expect("Worker ID");
    let other = WorkerId::parse("worker-02").expect("Worker ID");

    // A missing sidecar is an empty read surface, not an error.
    assert_eq!(sessions.session(&worker).expect("read"), None);

    sessions.record(&worker, "ses_one").expect("record");
    assert_eq!(
        sessions.session(&worker).expect("read").as_deref(),
        Some("ses_one")
    );

    // Each Worker's session is independent.
    sessions.record(&other, "ses_two").expect("record other");
    assert_eq!(
        sessions.session(&worker).expect("read").as_deref(),
        Some("ses_one")
    );
    assert_eq!(
        sessions.session(&other).expect("read").as_deref(),
        Some("ses_two")
    );

    // A later Run replaces the Worker's recorded session.
    sessions.record(&worker, "ses_three").expect("replace");
    assert_eq!(
        sessions.session(&worker).expect("read").as_deref(),
        Some("ses_three")
    );

    // It is jjfx-owned sidecar state, not part of the pool and Worker documents.
    let path = temp.path().join(".jj/pools/default/sessions.json");
    let value: Value =
        serde_json::from_slice(&fs::read(&path).expect("session sidecar")).expect("JSON");
    assert_eq!(value["sessions"]["worker-01"], "ses_three");
    assert_eq!(value["sessions"]["worker-02"], "ses_two");
    assert!(!temp.path().join(".jj/pools/default/pool.json").exists());
}

#[test]
fn a_legacy_repository_converts_to_a_directory_per_pool_on_first_access() {
    let (_temp, root) = legacy_repository();
    let repository = Repository::open(&root).expect("open converts the legacy layout");

    // The pool document and every pool-scoped document moved under the pool.
    let pool = root.join(".jj/pools").join(DEFAULT_POOL_NAME);
    assert_eq!(
        fs::read(pool.join("pool.json")).expect("pool document"),
        fixture("pool-empty.json")
    );
    assert_eq!(
        fs::read(pool.join("worker-01.json")).expect("Worker"),
        fixture("worker-idle-claude.json")
    );
    assert!(pool.join("waiting.json").exists());
    assert!(pool.join("sessions.json").exists());
    assert!(pool.join("runs.jsonl").exists());
    assert!(pool.join(".dispatch.lock").exists());

    // The repository-scoped documents moved to the root of `.jj/`.
    assert!(root.join(".jj/in-use.json").exists());
    assert!(root.join(".jj/undo.json").exists());

    // The legacy layout is gone and no index was written.
    assert!(!root.join(".jj/pool.json").exists());
    assert!(!root.join(".jj/pool").exists());
    assert!(!root.join(".jj/pools.json").exists());

    // The converted state reads back through the new layout.
    assert!(matches!(
        repository.state_store().pool().load().expect("pool"),
        Loaded::Present(_)
    ));
    assert_eq!(repository.state_store().waiting().held(), ["ENG-1"]);
    let worker = WorkerId::parse("worker-01").expect("Worker ID");
    assert_eq!(
        repository
            .state_store()
            .sessions()
            .session(&worker)
            .expect("session")
            .as_deref(),
        Some("ses_one")
    );
}

#[test]
fn a_conversion_that_cannot_be_verified_leaves_the_legacy_layout_intact() {
    let (_temp, root) = legacy_repository();
    // A destination that already holds different bytes cannot be verified
    // against the source. jjfx cannot know which copy is authoritative, so it
    // must refuse rather than overwrite either - and it must leave the legacy
    // layout exactly as it found it.
    let destination = root.join(".jj/pools").join(DEFAULT_POOL_NAME);
    fs::create_dir_all(&destination).expect("destination pool directory");
    let conflicting = b"{ \"size\": 3, \"workers\": [] }";
    fs::write(destination.join("pool.json"), conflicting).expect("conflicting document");

    let error = Repository::open(&root).expect_err("an unverifiable conversion must fail loudly");
    assert!(matches!(error, RepositoryError::StateConversion { .. }));

    assert_eq!(
        fs::read(root.join(".jj/pool.json")).expect("legacy document"),
        fixture("pool-empty.json"),
        "the legacy pool document must survive an unverifiable conversion"
    );
    assert!(
        root.join(".jj/pool/worker-01.json").exists(),
        "the legacy pool directory must survive an unverifiable conversion"
    );
    assert_eq!(
        fs::read(destination.join("pool.json")).expect("destination document"),
        conflicting,
        "a conflicting destination must not be overwritten"
    );
}

#[test]
fn an_orphan_pool_directory_is_reported_and_never_adopted() {
    let (_temp, repository) = repository();
    let ghost = repository.root().join(".jj/pools/ghost");
    fs::create_dir_all(&ghost).expect("orphan directory");
    fs::write(ghost.join("pool.json"), fixture("pool-empty.json")).expect("orphan document");

    let declared = BTreeSet::from([PoolName::default()]);
    assert_eq!(
        repository
            .orphan_pool_directories(&declared)
            .expect("orphans"),
        vec![PoolName::parse("ghost").expect("pool name")]
    );

    // The orphan is not a pool: the declared pool still has no state, and no
    // index was read or written.
    assert!(matches!(
        repository.state_store().pool().load().expect("pool"),
        Loaded::Missing
    ));
    assert!(!repository.root().join(".jj/pools.json").exists());
}

#[test]
fn a_declared_pool_without_a_directory_materialises_on_first_use() {
    let (temp, repository) = repository();
    let features = PoolName::parse("features").expect("pool name");
    let state: PoolState = serde_json::from_slice(&fixture("pool-empty.json")).expect("pool");
    repository
        .state_store_for_pool(features.clone())
        .pool()
        .commit(Expected::Missing, StateChange::Replace(state))
        .expect("create pool state");

    assert!(temp.path().join(".jj/pools/features/pool.json").exists());
    assert!(
        repository
            .orphan_pool_directories(&BTreeSet::from([features]))
            .expect("orphans")
            .is_empty()
    );
}
