use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

fn deserialize_nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::<T>::deserialize(deserializer)?))
}

#[derive(Debug, Deserialize, Serialize)]
struct PoolDocument {
    version: u8,
    workers: Vec<PoolWorker>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PoolWorker {
    worker_id: String,
    workspace: String,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct WorkerDocument {
    version: u8,
    worker_id: String,
    alias: String,
    workspace: String,
    status: String,
    #[serde(
        default,
        deserialize_with = "deserialize_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    ticket: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "deserialize_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    run: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "deserialize_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    agent_runtime: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "deserialize_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    started_at: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "deserialize_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    last_activity_at: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "deserialize_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    error: Option<Option<String>>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct DispatchGroupDocument {
    version: u8,
    parent_ticket: String,
    status: String,
    sub_issues: Vec<SubIssue>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SubIssue {
    ticket: String,
    status: String,
    blocked_by: Vec<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    worker_id: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "deserialize_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    run: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attempts: Option<u32>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/compatibility");

fn fixture(name: &str) -> PathBuf {
    Path::new(FIXTURES).join(name)
}

fn json_fixture(name: &str) -> Value {
    let path = fixture(name);
    let bytes = fs::read(&path)
        .unwrap_or_else(|error| panic!("fixture {} should be readable: {error}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "fixture {} should contain valid JSON: {error}",
            path.display()
        )
    })
}

#[test]
fn typed_wire_values_round_trip_without_losing_fields_or_presence() {
    for name in ["pool-empty.json", "pool-workers.json"] {
        let source = json_fixture(name);
        let typed: PoolDocument = serde_json::from_value(source.clone())
            .unwrap_or_else(|error| panic!("{name} should deserialize: {error}"));
        assert_eq!(
            serde_json::to_value(typed).expect("pool should serialize"),
            source,
            "round trip for {name}"
        );
    }

    for name in [
        "worker-idle-claude.json",
        "worker-busy-claude.json",
        "worker-done-codex.json",
        "worker-failed-codex.json",
        "worker-legacy-omits-runtime.json",
    ] {
        let source = json_fixture(name);
        let typed: WorkerDocument = serde_json::from_value(source.clone())
            .unwrap_or_else(|error| panic!("{name} should deserialize: {error}"));
        assert_eq!(
            serde_json::to_value(typed).expect("worker should serialize"),
            source,
            "round trip for {name}"
        );
    }

    for name in [
        "dispatch-pending.json",
        "dispatch-dispatched.json",
        "dispatch-retried.json",
        "dispatch-done.json",
        "dispatch-failed.json",
        "dispatch-merged.json",
    ] {
        let source = json_fixture(name);
        let typed: DispatchGroupDocument = serde_json::from_value(source.clone())
            .unwrap_or_else(|error| panic!("{name} should deserialize: {error}"));
        assert_eq!(
            serde_json::to_value(typed).expect("dispatch group should serialize"),
            source,
            "round trip for {name}"
        );
    }
}

#[test]
fn pool_fixtures_cover_empty_and_worker_references() {
    let empty = json_fixture("pool-empty.json");
    assert_eq!(empty["version"], 1);
    assert_eq!(empty["workers"], Value::Array(Vec::new()));

    let populated = json_fixture("pool-workers.json");
    let workers = populated["workers"]
        .as_array()
        .expect("pool workers should be an array");
    assert_eq!(workers.len(), 4);
    assert!(workers.iter().all(|worker| worker["worker_id"].is_string()));
}

#[test]
fn worker_fixtures_cover_statuses_and_both_agent_runtimes() {
    let cases = [
        ("worker-idle-claude.json", "idle", Some("claude")),
        ("worker-busy-claude.json", "busy", Some("claude")),
        ("worker-done-codex.json", "done", Some("codex")),
        ("worker-failed-codex.json", "failed", Some("codex")),
    ];

    for (name, status, runtime) in cases {
        let worker = json_fixture(name);
        assert_eq!(worker["status"], status, "status in {name}");
        let expected_runtime = runtime.map(Value::from).unwrap_or(Value::Null);
        assert_eq!(
            worker["agent_runtime"], expected_runtime,
            "runtime in {name}"
        );
        assert!(worker["worker_id"].is_string(), "worker id in {name}");
        assert!(worker["workspace"].is_string(), "workspace in {name}");
    }
}

#[test]
fn absent_worker_values_are_explicit_null_while_legacy_omissions_remain_visible() {
    let idle = json_fixture("worker-idle-claude.json");
    for field in ["ticket", "run", "started_at", "last_activity_at"] {
        assert!(idle.get(field).is_some(), "{field} must be present");
        assert!(idle[field].is_null(), "{field} must be explicit null");
    }

    let legacy = json_fixture("worker-legacy-omits-runtime.json");
    assert!(legacy.get("agent_runtime").is_none());
    assert_eq!(legacy["status"], "idle");
}

#[test]
fn dispatch_group_fixtures_cover_each_persisted_status() {
    let cases = [
        ("dispatch-pending.json", "pending"),
        ("dispatch-dispatched.json", "dispatched"),
        ("dispatch-retried.json", "retried"),
        ("dispatch-done.json", "done"),
        ("dispatch-failed.json", "failed"),
        ("dispatch-merged.json", "merged"),
    ];

    for (name, status) in cases {
        let group = json_fixture(name);
        assert_eq!(group["status"], status, "status in {name}");
        assert!(group["parent_ticket"].is_string(), "parent in {name}");
        assert!(group["sub_issues"].is_array(), "sub-issues in {name}");
    }
}

#[test]
fn dispatch_group_sub_issue_statuses_cover_terminal_and_retry_details() {
    let cases = [
        ("dispatch-pending.json", "pending", None),
        ("dispatch-dispatched.json", "dispatched", None),
        ("dispatch-retried.json", "retried", Some(1)),
        ("dispatch-done.json", "done", None),
        ("dispatch-failed.json", "failed", Some(2)),
        ("dispatch-merged.json", "merged", None),
    ];

    for (name, status, attempts) in cases {
        let group = json_fixture(name);
        let sub_issue = &group["sub_issues"][0];
        assert_eq!(sub_issue["status"], status, "sub-issue status in {name}");
        match attempts {
            Some(attempts) => assert_eq!(sub_issue["attempts"], attempts),
            None => assert!(sub_issue.get("attempts").is_none()),
        }
    }
}

#[test]
fn dispatch_group_sub_issue_statuses_keep_optional_fields_distinct() {
    let dispatched = json_fixture("dispatch-dispatched.json");
    let sub_issue = &dispatched["sub_issues"][0];
    assert_eq!(sub_issue["status"], "dispatched");
    assert!(sub_issue["worker_id"].is_string());
    assert!(sub_issue["run"].is_string());

    let pending = json_fixture("dispatch-pending.json");
    let sub_issue = &pending["sub_issues"][0];
    assert_eq!(sub_issue["status"], "pending");
    assert!(sub_issue["worker_id"].is_null());
    assert!(sub_issue["run"].is_null());
}

#[test]
fn ws_cache_fixtures_preserve_bytes_and_expose_invalid_lines() {
    let ordered = fs::read(fixture("ws-cache-ordered.txt"))
        .unwrap_or_else(|error| panic!("ordered cache fixture should be readable: {error}"));
    assert_eq!(
        ordered,
        b"default\t/repository\nfeature-a\t/repository-feature-a\nfeature-b\t/repository-feature-b\n"
    );

    let whitespace = fs::read_to_string(fixture("ws-cache-whitespace.txt"))
        .unwrap_or_else(|error| panic!("whitespace cache fixture should be readable: {error}"));
    assert!(whitespace.contains("feature with spaces\t/repository/feature with spaces\n"));
    assert!(whitespace.contains("leading\t /repository/with-a-leading-space\n"));

    let malformed = fs::read_to_string(fixture("ws-cache-malformed.txt"))
        .unwrap_or_else(|error| panic!("malformed cache fixture should be readable: {error}"));
    assert!(malformed.lines().any(|line| !line.contains('\t')));
    assert!(malformed.lines().any(|line| line.ends_with('\t')));
    assert!(malformed.lines().any(|line| line.starts_with('\t')));
}

#[test]
fn missing_ws_cache_is_an_empty_optional_surface() {
    let missing = fixture("ws-cache-missing.txt");
    assert!(!missing.exists(), "missing fixture must remain absent");
}
