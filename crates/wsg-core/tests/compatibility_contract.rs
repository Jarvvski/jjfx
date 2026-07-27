use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/compatibility");

fn fixture(name: &str) -> PathBuf {
    Path::new(FIXTURES).join(name)
}
fn json_fixture(name: &str) -> Value {
    serde_json::from_slice(&fs::read(fixture(name)).expect("fixture should be readable"))
        .expect("fixture should contain valid JSON")
}

#[test]
fn pool_fixtures_match_go_wsg_shape() {
    let empty = json_fixture("pool-empty.json");
    assert_eq!(empty["size"], 0);
    assert_eq!(empty["workers"], Value::Array(Vec::new()));
    assert!(empty.get("version").is_none());

    let populated = json_fixture("pool-workers.json");
    assert_eq!(populated["size"], 4);
    assert_eq!(populated["workers"][1], "worker-02");
    assert_eq!(populated["names"]["worker-02"], "beta");
}

#[test]
fn worker_fixtures_match_go_wsg_nullability_and_runtime_fields() {
    for (name, status, agent) in [
        ("worker-idle-claude.json", "idle", "claude"),
        ("worker-busy-claude.json", "busy", "claude"),
        ("worker-done-codex.json", "done", "codex"),
        ("worker-failed-codex.json", "failed", "codex"),
    ] {
        let worker = json_fixture(name);
        assert_eq!(worker["status"], status);
        assert_eq!(worker["agent"], agent);
        for field in [
            "ticket",
            "pid",
            "started_at",
            "completed_at",
            "log_file",
            "branch_name",
            "exit_code",
            "error",
        ] {
            assert!(worker.get(field).is_some(), "{field} missing in {name}");
        }
        assert!(worker.get("worker_id").is_none());
        assert!(worker.get("workspace").is_none());
    }
    assert!(
        json_fixture("worker-legacy-omits-runtime.json")
            .get("agent")
            .is_none()
    );
}

#[test]
fn dispatch_group_fixtures_use_a_ticket_keyed_map_and_go_statuses() {
    for (name, status, retries) in [
        ("dispatch-pending.json", "pending", 0),
        ("dispatch-dispatched.json", "dispatched", 0),
        ("dispatch-done.json", "done", 0),
        ("dispatch-failed.json", "failed", 1),
        ("dispatch-skipped.json", "skipped", 0),
    ] {
        let group = json_fixture(name);
        assert_eq!(group["parent"], "ENG-100");
        assert!(group["sub_issues"].is_object());
        assert_eq!(group["sub_issues"]["ENG-101"]["status"], status);
        assert_eq!(group["sub_issues"]["ENG-101"]["retries"], retries);
    }
}

#[test]
fn ws_cache_fixtures_preserve_bytes_and_expose_invalid_lines() {
    let ordered = fs::read(fixture("ws-cache-ordered.txt")).expect("ordered fixture");
    assert_eq!(ordered, b"default\t/repository\nfeature-a\t/repository-feature-a\nfeature-b\t/repository-feature-b\n");
    let whitespace =
        fs::read_to_string(fixture("ws-cache-whitespace.txt")).expect("whitespace fixture");
    assert!(whitespace.contains("feature with spaces\t/repository/feature with spaces\n"));
    let malformed =
        fs::read_to_string(fixture("ws-cache-malformed.txt")).expect("malformed fixture");
    assert!(malformed.lines().any(|line| !line.contains('\t')));
}

#[test]
fn missing_ws_cache_is_an_empty_optional_surface() {
    assert!(!fixture("ws-cache-missing.txt").exists());
}
