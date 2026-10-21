use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use jjfx::engine::{
    CommitOutcome, DEFAULT_POOL_NAME, DispatchGroupState, Expected, Loaded, PoolCapacity, PoolName,
    PoolState, Repository, RepositoryError, RunRecordUsage, SessionBaseline, StateChange, TicketId,
    WireStatus, WireTimestamp, WorkerId, WorkerState,
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

    // Fail the manifest write *specifically*. The manifest's atomic replacement
    // creates its temporary file in the pool directory, so a read-only pool
    // directory fails it - but the cleanup markers live in that same directory
    // now, so a read-only directory would fail the marker write too and the
    // assertion below would hold for the wrong reason.
    //
    // Point the marker this shrink would write at a writable file *outside* the
    // pool directory instead: the manifest write still fails, but an attempted
    // marker write would now succeed and leave a marker behind, which is what
    // the assertion catches. Production decides whether a marker is present with
    // `Path::exists()`, which follows the link, so the dangling link reads as
    // "no marker" - to the code and to this assertion alike.
    let pool_directory = temp.path().join(".jj/pools/default");
    let marker = pool_directory.join("worker-02.cleanup");
    let escaped_marker = temp.path().join("escaped-worker-02.cleanup");
    std::os::unix::fs::symlink(&escaped_marker, &marker).expect("marker link");

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
    assert_eq!(sessions.baseline(&worker).expect("read baseline"), None);

    let baseline = SessionBaseline {
        cost_micro_usd: 1_500,
        usage: RunRecordUsage {
            input_tokens: 10,
            cached_input_tokens: 1,
            cache_write_input_tokens: 2,
            output_tokens: 3,
            reasoning_output_tokens: 4,
        },
    };
    sessions
        .record(&worker, "ses_one", baseline)
        .expect("record");
    assert_eq!(
        sessions.session(&worker).expect("read").as_deref(),
        Some("ses_one")
    );
    assert_eq!(
        sessions.baseline(&worker).expect("read baseline"),
        Some(baseline)
    );

    // Each Worker's session is independent.
    sessions
        .record(&other, "ses_two", SessionBaseline::default())
        .expect("record other");
    assert_eq!(
        sessions.session(&worker).expect("read").as_deref(),
        Some("ses_one")
    );
    assert_eq!(
        sessions.session(&other).expect("read").as_deref(),
        Some("ses_two")
    );

    // A later Run replaces the Worker's recorded session and its baseline.
    sessions
        .record(&worker, "ses_three", SessionBaseline::default())
        .expect("replace");
    assert_eq!(
        sessions.session(&worker).expect("read").as_deref(),
        Some("ses_three")
    );
    assert_eq!(
        sessions.baseline(&worker).expect("read baseline"),
        Some(SessionBaseline::default())
    );

    // Clearing a Worker forgets both the session and its baseline.
    sessions.clear(&worker).expect("clear");
    assert_eq!(sessions.session(&worker).expect("read"), None);
    assert_eq!(sessions.baseline(&worker).expect("read baseline"), None);

    // It is jjfx-owned sidecar state, not part of the pool and Worker documents.
    let path = temp.path().join(".jj/pools/default/sessions.json");
    let value: Value =
        serde_json::from_slice(&fs::read(&path).expect("session sidecar")).expect("JSON");
    assert_eq!(value["sessions"]["worker-02"], "ses_two");
    assert!(!temp.path().join(".jj/pools/default/pool.json").exists());
}

#[test]
fn stack_base_sidecar_records_reads_and_replaces_worker_bases() {
    let (temp, repository) = repository();
    let bases = repository.state_store().stack_bases();
    let worker = WorkerId::parse("worker-01").expect("Worker ID");
    let other = WorkerId::parse("worker-02").expect("Worker ID");
    let blocker = "de8626600e67f236e9a3db9f9c93ebe7273843d1".to_owned();

    // A missing sidecar is an empty read surface, not an error: no base recorded
    // is the unstacked Worker, which the trunk exclusion already covers.
    assert_eq!(bases.base(&worker).expect("read"), None);

    bases
        .record(&worker, vec![blocker.clone()])
        .expect("record");
    assert_eq!(
        bases.base(&worker).expect("read"),
        Some(vec![blocker.clone()])
    );

    // Each Worker's stack base is independent.
    bases
        .record(
            &other,
            vec!["f6867b5be87dcfdd1951ab4506439563af0abfab".to_owned()],
        )
        .expect("record other");
    assert_eq!(
        bases.base(&worker).expect("read"),
        Some(vec![blocker.clone()])
    );
    assert_eq!(
        bases.base(&other).expect("read"),
        Some(vec!["f6867b5be87dcfdd1951ab4506439563af0abfab".to_owned()])
    );

    // A Worker re-dispatched on a different Blocker replaces its base rather than
    // keeping the stale one, which would exclude a commit it never sat on.
    bases
        .record(
            &worker,
            vec!["1111111111111111111111111111111111111111".to_owned()],
        )
        .expect("replace");
    assert_eq!(
        bases.base(&worker).expect("read"),
        Some(vec!["1111111111111111111111111111111111111111".to_owned()])
    );

    // It is jjfx-owned sidecar state, not part of the pool and Worker documents.
    let path = temp.path().join(".jj/pools/default/stack-bases.json");
    let value: Value =
        serde_json::from_slice(&fs::read(&path).expect("stack base sidecar")).expect("JSON");
    assert_eq!(
        value["bases"]["worker-01"][0],
        "1111111111111111111111111111111111111111"
    );
    assert_eq!(
        value["bases"]["worker-02"][0],
        "f6867b5be87dcfdd1951ab4506439563af0abfab"
    );
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
