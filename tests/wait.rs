//! `jjfx wait` end to end: the exit code is the answer.
//!
//! Each case drives the real binary against a throwaway `jj` repository and a
//! hand-written Worker Pool, then reads the process exit code. The command
//! blocks by design, so every invocation is run through [`run_bounded`], which
//! kills a `wait` that never returns rather than hanging the suite.

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// A hang guard, not a latency budget: `wait` blocks until its condition, so a
/// test that never reaches one would otherwise run forever. Generous, because
/// the failure it guards against is a hang.
const WAIT_LIVENESS_TIMEOUT: Duration = Duration::from_secs(30);

/// Runs `command`, killing and failing if it does not return within `timeout`.
///
/// `Child::wait_with_output` blocks, so the child is polled instead; the pipes
/// are read only once it has exited, which is safe because `wait` writes little.
fn run_bounded(mut command: Command, timeout: Duration) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("jjfx should spawn");
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait().expect("poll jjfx").is_some() {
            return child.wait_with_output().expect("jjfx output");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("jjfx wait did not return within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// A `jj` repository plus an empty config home, the shape a CLI invocation needs.
/// A missing config file is jjfx's defaults.
fn jj_repository() -> (TempDir, TempDir) {
    let repository = TempDir::new().expect("temporary repository");
    let status = Command::new("jj")
        .args([
            "--config",
            "signing.behavior=drop",
            "git",
            "init",
            "--no-colocate",
        ])
        .arg(repository.path())
        .status()
        .expect("jj should be installed");
    assert!(status.success(), "jj git init");
    let config_home = TempDir::new().expect("temporary config home");
    (repository, config_home)
}

/// Writes the Pool manifest naming `workers`.
fn write_manifest(root: &Path, workers: &[&str]) {
    fs::create_dir_all(root.join(".jj").join("pool")).expect("pool directory");
    let manifest = serde_json::json!({
        "created_at": "2026-07-27T10:00:00Z",
        "gh_repo": "Jarvvski/jjfx",
        "names": {},
        "size": workers.len(),
        "workers": workers,
    });
    fs::write(
        root.join(".jj").join("pool.json"),
        serde_json::to_vec(&manifest).expect("manifest json"),
    )
    .expect("pool manifest");
}

/// Writes one Worker state document with the given status and Ticket.
fn write_worker(root: &Path, id: &str, status: &str, ticket: Option<&str>) {
    fs::create_dir_all(root.join(".jj").join("pool")).expect("pool directory");
    let exit_code = match status {
        "done" => Some(0),
        "failed" => Some(1),
        _ => None,
    };
    let state = serde_json::json!({
        "agent": "claude",
        "branch_name": format!("{id}-branch"),
        "completed_at": matches!(status, "done" | "failed").then_some("2026-07-27T10:10:00Z"),
        "error": (status == "failed").then_some("the Run failed"),
        "exit_code": exit_code,
        "log_file": format!("/repo/.jj/pool/{id}.log"),
        "pid": null,
        "started_at": (status != "idle").then_some("2026-07-27T10:02:00Z"),
        "status": status,
        "ticket": ticket,
    });
    fs::write(
        root.join(".jj").join("pool").join(format!("{id}.json")),
        serde_json::to_vec(&state).expect("worker json"),
    )
    .expect("worker state");
}

/// Appends one succeeded Run Record, the durable completion signal a local
/// Ticket writeback resolves from.
fn write_run_record(root: &Path, worker: &str, ticket: &str) {
    fs::create_dir_all(root.join(".jj").join("pool")).expect("pool directory");
    let line = format!(
        "{{\"run_id\":\"{worker}@2026-01-01T00:00:00Z\",\"worker\":\"{worker}\",\
         \"ticket\":\"{ticket}\",\"conclusion\":\"succeeded\"}}\n"
    );
    fs::write(root.join(".jj").join("pool").join("runs.jsonl"), line).expect("run record");
}

/// Writes one local Ticket file under `<root>/.scratch/<feature>/issues/`.
fn write_ticket(root: &Path, feature: &str, file: &str, content: &str) {
    let directory = root.join(".scratch").join(feature).join("issues");
    fs::create_dir_all(&directory).expect("issues directory");
    fs::write(directory.join(file), content).expect("Ticket file");
}

/// Reads one local Ticket file's contents.
fn ticket_status(root: &Path, feature: &str, file: &str) -> String {
    fs::read_to_string(
        root.join(".scratch")
            .join(feature)
            .join("issues")
            .join(file),
    )
    .expect("Ticket file")
}

/// A `jjfx wait` invocation against `repository`, with `config_home` as XDG home.
fn wait_command(repository: &Path, config_home: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jjfx"));
    command
        .arg("wait")
        .args(args)
        .current_dir(repository)
        .env("XDG_CONFIG_HOME", config_home);
    command
}

#[test]
fn wait_returns_immediately_for_an_already_terminal_worker() {
    let (repository, config_home) = jj_repository();
    write_manifest(repository.path(), &["worker-1"]);
    write_worker(repository.path(), "worker-1", "done", Some("ENG-1"));

    // No --timeout: a Worker already in the waited-for state must return at once,
    // so a passing run proves there is no lost wakeup.
    let output = run_bounded(
        wait_command(repository.path(), config_home.path(), &["1"]),
        WAIT_LIVENESS_TIMEOUT,
    );

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("worker-1 done"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn wait_times_out_with_a_code_distinct_from_success() {
    let (repository, config_home) = jj_repository();
    write_manifest(repository.path(), &["worker-1"]);
    // An idle Worker has not reached a terminal state and, with no Run, never
    // will: the wait must give up on time.
    write_worker(repository.path(), "worker-1", "idle", None);

    let output = run_bounded(
        wait_command(
            repository.path(),
            config_home.path(),
            &["1", "--timeout", "1"],
        ),
        WAIT_LIVENESS_TIMEOUT,
    );

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("timed out"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_worker_that_fails_while_waiting_for_done_is_not_a_timeout() {
    let (repository, config_home) = jj_repository();
    write_manifest(repository.path(), &["worker-1"]);
    write_worker(repository.path(), "worker-1", "failed", Some("ENG-1"));

    // No --timeout: a failed Worker must decide the wait at once with code 2 -
    // the terminal-but-unsatisfied answer - not block until a timeout.
    let output = run_bounded(
        wait_command(
            repository.path(),
            config_home.path(),
            &["1", "--until", "done"],
        ),
        WAIT_LIVENESS_TIMEOUT,
    );

    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("does not satisfy"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn wait_all_returns_once_every_worker_is_terminal() {
    let (repository, config_home) = jj_repository();
    write_manifest(repository.path(), &["worker-1", "worker-2"]);
    write_worker(repository.path(), "worker-1", "done", Some("ENG-1"));
    write_worker(repository.path(), "worker-2", "failed", Some("ENG-2"));

    let output = run_bounded(
        wait_command(repository.path(), config_home.path(), &["--all"]),
        WAIT_LIVENESS_TIMEOUT,
    );

    assert_eq!(output.status.code(), Some(0), "{output:?}");
}

#[test]
fn a_multi_worker_wait_does_not_return_until_every_worker_is_done() {
    let (repository, config_home) = jj_repository();
    write_manifest(repository.path(), &["worker-1", "worker-2"]);
    write_worker(repository.path(), "worker-1", "done", Some("ENG-1"));
    write_worker(repository.path(), "worker-2", "idle", None);

    // One Worker is done, but the set is not: the wait must not return 0 early.
    let output = run_bounded(
        wait_command(
            repository.path(),
            config_home.path(),
            &["1", "2", "--until", "done", "--timeout", "1"],
        ),
        WAIT_LIVENESS_TIMEOUT,
    );

    assert_eq!(output.status.code(), Some(1), "{output:?}");
}

#[test]
fn a_failure_in_the_set_decides_a_multi_worker_wait() {
    let (repository, config_home) = jj_repository();
    write_manifest(repository.path(), &["worker-1", "worker-2"]);
    write_worker(repository.path(), "worker-1", "done", Some("ENG-1"));
    write_worker(repository.path(), "worker-2", "failed", Some("ENG-2"));

    // Waiting for `done` over a set that contains a failed Worker is code 2, not
    // a timeout: one failure decides the verdict.
    let output = run_bounded(
        wait_command(
            repository.path(),
            config_home.path(),
            &["1", "2", "--until", "done"],
        ),
        WAIT_LIVENESS_TIMEOUT,
    );

    assert_eq!(output.status.code(), Some(2), "{output:?}");
}

#[test]
fn wait_json_prints_the_worker_object_status_json_uses() {
    let (repository, config_home) = jj_repository();
    write_manifest(repository.path(), &["worker-1"]);
    write_worker(repository.path(), "worker-1", "done", Some("ENG-1"));

    let output = run_bounded(
        wait_command(repository.path(), config_home.path(), &["1", "--json"]),
        WAIT_LIVENESS_TIMEOUT,
    );

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("wait --json is valid JSON");
    let workers = value["workers"].as_array().expect("workers array");
    assert_eq!(workers.len(), 1, "{value}");
    let worker = &workers[0];
    assert_eq!(worker["worker"], "worker-1");
    assert_eq!(worker["status"], "done");
    assert_eq!(worker["ticket"], "ENG-1");
    // The per-Worker object carries the same fields `status --json` emits.
    for field in [
        "worker",
        "name",
        "status",
        "stuck",
        "ticket",
        "elapsed",
        "error",
        "exit_code",
        "log_file",
        "cost_usd",
    ] {
        assert!(worker.get(field).is_some(), "missing {field} in {worker}");
    }
}

#[test]
fn wait_ticket_resolved_confirms_end_to_end() {
    let (repository, config_home) = jj_repository();
    fs::create_dir_all(config_home.path().join("jjfx")).expect("config directory");
    fs::write(
        config_home.path().join("jjfx").join("config.toml"),
        "[tickets]\nsource = \"local\"\n",
    )
    .expect("config file");
    write_ticket(
        repository.path(),
        "widgets",
        "01-foundation.md",
        "# Foundation\n\nStatus: claimed\n",
    );
    write_manifest(repository.path(), &["worker-1"]);
    write_worker(
        repository.path(),
        "worker-1",
        "done",
        Some("widgets--01-foundation"),
    );
    write_run_record(repository.path(), "worker-1", "widgets--01-foundation");

    let output = run_bounded(
        wait_command(
            repository.path(),
            config_home.path(),
            &["1", "--until", "ticket-resolved"],
        ),
        WAIT_LIVENESS_TIMEOUT,
    );

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        ticket_status(repository.path(), "widgets", "01-foundation.md")
            .contains("Status: resolved"),
        "the wait must reconcile the finished Run's Ticket to resolved"
    );
}

#[test]
fn wait_rejects_an_unknown_worker_rather_than_blocking() {
    let (repository, config_home) = jj_repository();
    write_manifest(repository.path(), &["worker-1"]);
    write_worker(repository.path(), "worker-1", "done", None);

    let output = run_bounded(
        wait_command(repository.path(), config_home.path(), &["9"]),
        WAIT_LIVENESS_TIMEOUT,
    );

    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no such Worker"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
