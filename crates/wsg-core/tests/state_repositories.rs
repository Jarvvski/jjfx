use std::fs;
use std::path::Path;

use serde_json::Value;
use tempfile::TempDir;
use wsg_core::{
    CommitOutcome, DispatchGroupState, Expected, Loaded, PoolState, Repository, StateChange,
    TicketId, WorkerId, WorkerState,
};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/compatibility");
fn fixture(name: &str) -> Vec<u8> {
    fs::read(Path::new(FIXTURES).join(name)).expect("fixture")
}
fn repository() -> (TempDir, Repository) {
    let temp = tempfile::tempdir().expect("temp repository");
    fs::create_dir(temp.path().join(".jj")).expect("repository marker");
    let repository = Repository::open(temp.path()).expect("repository");
    (temp, repository)
}

#[test]
fn pool_repository_creates_replaces_conflicts_and_removes() {
    let (_temp, repository) = repository();
    let pool = repository.state_store().pool();
    assert!(matches!(pool.load().expect("load"), Loaded::Missing));
    let state: PoolState = serde_json::from_slice(&fixture("pool-empty.json")).expect("Go pool");
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
    let path = temp.path().join(".jj/pool/worker-01.json");
    fs::create_dir_all(path.parent().expect("parent")).expect("pool directory");
    let mut source: Value =
        serde_json::from_slice(&fixture("worker-busy-claude.json")).expect("Go Worker");
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
    assert!(temp.path().join(".jj/pool/dispatch-eng-100.json").exists());
    assert!(temp.path().join(".jj/pool/.dispatch.lock").exists());
    assert!(
        temp.path()
            .join(".jj/pool/dispatch-eng-100.json.lock")
            .exists()
    );
}

#[test]
fn invalid_replacement_preserves_previous_valid_file() {
    let (temp, repository) = repository();
    let pool = repository.state_store().pool();
    let state: PoolState = serde_json::from_slice(&fixture("pool-empty.json")).expect("Go pool");
    pool.commit(Expected::Missing, StateChange::Replace(state))
        .expect("create");
    let path = temp.path().join(".jj/pool.json");
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
    fs::write(temp.path().join(".jj/pool.json"), b"{ malformed").expect("malformed pool");
    assert!(repository.state_store().pool().load().is_err());
}

#[test]
fn persisted_identifiers_are_validated_before_path_construction() {
    let (temp, repository) = repository();
    fs::write(
        temp.path().join(".jj/pool.json"),
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
    assert!(!temp.path().join(".jj/pool/dispatch-eng-100.json").exists());
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
    let mut pool: PoolState = serde_json::from_slice(&fixture("pool-empty.json")).expect("Go pool");
    pool.agent = Some(wsg_core::WireAgent::new(""));
    repository
        .state_store()
        .pool()
        .commit(Expected::Missing, StateChange::Replace(pool))
        .expect("pool commit");
    let pool_json: Value =
        serde_json::from_slice(&fs::read(temp.path().join(".jj/pool.json")).expect("pool state"))
            .expect("pool JSON");
    assert!(pool_json.get("agent").is_none());

    let worker_id = WorkerId::parse("worker-01").expect("Worker ID");
    let mut worker = WorkerState::new(wsg_core::WireStatus::new("idle"));
    worker.branch_name = Some(String::new());
    repository
        .state_store()
        .worker(worker_id)
        .commit(Expected::Missing, StateChange::Replace(worker))
        .expect("Worker commit");
    let worker_json: Value = serde_json::from_slice(
        &fs::read(temp.path().join(".jj/pool/worker-01.json")).expect("Worker state"),
    )
    .expect("Worker JSON");
    assert!(worker_json["branch_name"].is_null());
}

#[test]
fn unknown_json_number_spelling_survives_a_worker_mutation() {
    let (temp, repository) = repository();
    let path = temp.path().join(".jj/pool/worker-01.json");
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
