//! `jjfx logs` end to end: follow mode ends with the Run.
//!
//! Each case drives the real binary against a throwaway `jj` repository and a
//! hand-written Worker Pool. Follow mode blocks while the Run is live, so every
//! invocation is run through a bounded wait that kills a `logs` which never
//! returns rather than hanging the suite.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use jjfx::test_support::LIVENESS_TIMEOUT;
use tempfile::TempDir;

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

/// Writes the Worker Pool manifest naming `workers`.
fn write_manifest(root: &Path, workers: &[&str]) {
    let pool = root.join(".jj").join("pools").join("default");
    fs::create_dir_all(&pool).expect("pool directory");
    let manifest = serde_json::json!({
        "created_at": "2026-07-27T10:00:00Z",
        "gh_repo": "Jarvvski/jjfx",
        "names": {},
        "size": workers.len(),
        "workers": workers,
    });
    fs::write(
        pool.join("pool.json"),
        serde_json::to_vec(&manifest).expect("manifest json"),
    )
    .expect("pool manifest");
}

/// Writes one Worker state document with the given status, pointing its log at
/// `<root>/.jj/pools/default/<id>.log` and writing `log` there when given.
///
/// The state is replaced by rename, so a reader never observes a half-written
/// document and mistakes the Worker for one that has left its Worker Pool.
fn write_worker(root: &Path, id: &str, status: &str, log: Option<&str>) {
    write_worker_state(root, id, "claude", status, log);
}

/// Writes one opencode Worker state, the runtime a session-delivered Run uses.
fn write_opencode_worker(root: &Path, id: &str, status: &str, log: Option<&str>) {
    write_worker_state(root, id, "opencode", status, log);
}

fn write_worker_state(root: &Path, id: &str, agent: &str, status: &str, log: Option<&str>) {
    let pool = root.join(".jj").join("pools").join("default");
    fs::create_dir_all(&pool).expect("pool directory");
    let log_path = pool.join(format!("{id}.log"));
    if let Some(contents) = log {
        fs::write(&log_path, contents).expect("worker log");
    }
    let state = serde_json::json!({
        "agent": agent,
        "branch_name": format!("{id}-branch"),
        "completed_at": matches!(status, "done" | "failed").then_some("2026-07-27T10:10:00Z"),
        "error": (status == "failed").then_some("the Run failed"),
        "exit_code": match status {
            "done" => Some(0),
            "failed" => Some(1),
            _ => None,
        },
        "log_file": log_path.to_string_lossy(),
        "pid": null,
        "started_at": (status != "idle").then_some("2026-07-27T10:02:00Z"),
        "status": status,
        "ticket": null,
    });
    let path = pool.join(format!("{id}.json"));
    let temporary = pool.join(format!("{id}.json.tmp"));
    fs::write(&temporary, serde_json::to_vec(&state).expect("worker json")).expect("worker state");
    fs::rename(&temporary, &path).expect("worker state rename");
}

/// Records the opencode session a Worker's Run drives, the way `dispatch` does,
/// so `logs` can read a Run that writes no process log.
fn write_worker_session(root: &Path, id: &str, session_id: &str) {
    let pool = root.join(".jj").join("pools").join("default");
    fs::create_dir_all(&pool).expect("pool directory");
    let sessions = serde_json::json!({ "sessions": { id: session_id } });
    fs::write(
        pool.join("sessions.json"),
        serde_json::to_vec(&sessions).expect("sessions json"),
    )
    .expect("worker sessions");
}

/// Writes a fake `opencode` answering the session API and returns a `PATH` with
/// its directory ahead of the inherited one, so the real `jj` is still found.
///
/// `messages` is the `session.message.list` body the fake serves; `session.get`
/// always reports an open session, so a `busy` Worker stays `busy` until its
/// state says otherwise.
fn fake_opencode(bin: &Path, messages: &str) -> std::ffi::OsString {
    fs::create_dir_all(bin).expect("fake opencode directory");
    let messages_path = bin.join("messages.json");
    fs::write(&messages_path, messages).expect("session messages");
    let script = format!(
        "#!/bin/sh\ncase \"$2\" in\n  session.message.list) cat '{}' ;;\n  session.get) printf '{{\"data\":{{\"id\":\"ses_test\"}}}}' ;;\n  *) exit 1 ;;\nesac\nexit 0\n",
        messages_path.display()
    );
    let opencode = bin.join("opencode");
    fs::write(&opencode, script).expect("fake opencode");
    let mut permissions = fs::metadata(&opencode)
        .expect("fake opencode metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&opencode, permissions).expect("fake opencode permissions");
    let mut paths = vec![bin.to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(paths).expect("PATH with the fake opencode")
}

/// A fake `opencode` whose API cannot be reached, so a session cannot be read.
fn unreachable_opencode(bin: &Path) -> std::ffi::OsString {
    fs::create_dir_all(bin).expect("fake opencode directory");
    let opencode = bin.join("opencode");
    fs::write(
        &opencode,
        "#!/bin/sh\nprintf 'Error: Could not reach server\\n' >&2\nexit 1\n",
    )
    .expect("fake opencode");
    let mut permissions = fs::metadata(&opencode)
        .expect("fake opencode metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&opencode, permissions).expect("fake opencode permissions");
    let mut paths = vec![bin.to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(paths).expect("PATH with the fake opencode")
}

/// One assistant message reporting a line of prose and a completed tool call.
fn session_messages() -> String {
    r#"{"data":[
        {"id":"msg_2","type":"assistant","content":[
            {"type":"text","text":"reading the ticket"},
            {"type":"tool","name":"bash","state":{"status":"completed","input":{"command":"cargo test"},"content":[]}}
        ]},
        {"id":"msg_1","type":"user","text":"Implement the ticket."}
    ],"cursor":{"previous":"p","next":"n"}}"#
        .to_owned()
}

/// Returns a Worker to idle the way a Reset does: status `idle` with the Run
/// fields cleared, including `log_file`. The log file the abandoned Run wrote is
/// left in place, as a Reset leaves it.
fn write_idle_worker(root: &Path, id: &str) {
    let pool = root.join(".jj").join("pools").join("default");
    fs::create_dir_all(&pool).expect("pool directory");
    let state = serde_json::json!({
        "agent": null,
        "branch_name": null,
        "completed_at": null,
        "error": null,
        "exit_code": null,
        "log_file": null,
        "pid": null,
        "started_at": null,
        "status": "idle",
        "ticket": null,
    });
    let path = pool.join(format!("{id}.json"));
    let temporary = pool.join(format!("{id}.json.tmp"));
    fs::write(&temporary, serde_json::to_vec(&state).expect("worker json")).expect("worker state");
    fs::rename(&temporary, &path).expect("worker state rename");
}

/// A `jjfx logs` invocation against `repository`, with `config_home` as XDG home.
fn logs_command(repository: &Path, config_home: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jjfx"));
    command
        .arg("logs")
        .args(args)
        .current_dir(repository)
        .env("XDG_CONFIG_HOME", config_home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// Waits for `child` to exit, killing and failing if it outlives `timeout`.
fn wait_bounded(child: &mut Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("poll jjfx") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("jjfx logs did not return within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Runs `command` to completion under the same bound as [`wait_bounded`].
fn run_bounded(mut command: Command, timeout: Duration) -> Output {
    let mut child = command.spawn().expect("jjfx should spawn");
    let _ = wait_bounded(&mut child, timeout);
    child.wait_with_output().expect("jjfx output")
}

/// Drains a child's stderr on its own thread and reports when the follow banner
/// appears, so a `logs` that never announces itself fails a bounded wait instead
/// of blocking the test on a pipe that will never produce a line.
struct FollowWatcher {
    banner: mpsc::Receiver<()>,
    handle: thread::JoinHandle<String>,
}

impl FollowWatcher {
    fn watch(stderr: impl Read + Send + 'static) -> Self {
        let (sender, banner) = mpsc::channel();
        let handle = thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut collected = String::new();
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if line.contains("Following") {
                    let _ = sender.send(());
                }
                collected.push_str(&line);
            }
            collected
        });
        Self { banner, handle }
    }

    /// Blocks until follow mode has announced itself, or fails if it never does.
    fn wait_for_banner(&self) {
        match self.banner.recv_timeout(LIVENESS_TIMEOUT) {
            Ok(()) => {}
            Err(RecvTimeoutError::Timeout) => {
                panic!("jjfx logs never announced follow mode within {LIVENESS_TIMEOUT:?}");
            }
            Err(RecvTimeoutError::Disconnected) => {
                panic!("jjfx logs exited without announcing follow mode");
            }
        }
    }

    /// Joins the reader and returns everything the child wrote to stderr.
    fn into_output(self) -> String {
        self.handle.join().expect("stderr reader")
    }
}

/// Spawns `jjfx logs` and returns the child plus a watcher over its stderr.
fn spawn_following(mut command: Command) -> (Child, FollowWatcher) {
    let mut child = command.spawn().expect("jjfx logs should spawn");
    let stderr = child.stderr.take().expect("stderr");
    (child, FollowWatcher::watch(stderr))
}

/// The Worker state path, for a test that removes it mid-follow.
fn worker_state(root: &Path, id: &str) -> std::path::PathBuf {
    root.join(".jj")
        .join("pools")
        .join("default")
        .join(format!("{id}.json"))
}

/// The Worker log path, for a test that removes it mid-follow.
fn worker_log(root: &Path, id: &str) -> std::path::PathBuf {
    root.join(".jj")
        .join("pools")
        .join("default")
        .join(format!("{id}.log"))
}

#[test]
fn logs_returns_when_a_running_worker_reaches_a_terminal_state() {
    let (repository, config_home) = jj_repository();
    write_manifest(repository.path(), &["worker-1"]);
    write_worker(repository.path(), "worker-1", "busy", Some("thinking\n"));

    let (mut child, watcher) =
        spawn_following(logs_command(repository.path(), config_home.path(), &["1"]));
    // Wait until follow mode is watching, so the Run genuinely ends under the
    // loop rather than before it attaches.
    watcher.wait_for_banner();

    // The Run ends. The follow loop must notice on its next pass and return
    // instead of following a log that will never grow again.
    write_worker(
        repository.path(),
        "worker-1",
        "done",
        Some("thinking\ndone\n"),
    );

    let status = wait_bounded(&mut child, LIVENESS_TIMEOUT);
    assert_eq!(status.code(), Some(0), "follow must return cleanly");

    let stderr = watcher.into_output();
    assert!(stderr.contains("Run is done"), "{stderr:?}");
}

#[test]
fn logs_returns_immediately_for_an_already_terminal_worker() {
    let (repository, config_home) = jj_repository();
    write_manifest(repository.path(), &["worker-1"]);
    write_worker(repository.path(), "worker-1", "done", Some("finished\n"));

    let output = run_bounded(
        logs_command(repository.path(), config_home.path(), &["1"]),
        LIVENESS_TIMEOUT,
    );

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("finished"),
        "the tail is still printed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    // There is nothing to follow, so the banner is not printed.
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("Following"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn logs_stops_cleanly_when_the_run_log_disappears_mid_follow() {
    let (repository, config_home) = jj_repository();
    write_manifest(repository.path(), &["worker-1"]);
    write_worker(repository.path(), "worker-1", "busy", Some("thinking\n"));

    let (mut child, watcher) =
        spawn_following(logs_command(repository.path(), config_home.path(), &["1"]));
    watcher.wait_for_banner();

    // The log vanishes under the follow: nothing is left to read, so the command
    // must end cleanly rather than error.
    fs::remove_file(worker_log(repository.path(), "worker-1")).expect("remove log");

    let status = wait_bounded(&mut child, LIVENESS_TIMEOUT);
    assert_eq!(status.code(), Some(0), "a vanished log is a clean end");

    let stderr = watcher.into_output();
    assert!(stderr.contains("the log is gone"), "{stderr:?}");
}

#[test]
fn logs_stops_cleanly_when_the_worker_leaves_the_pool() {
    let (repository, config_home) = jj_repository();
    write_manifest(repository.path(), &["worker-1"]);
    write_worker(repository.path(), "worker-1", "busy", Some("thinking\n"));

    let (mut child, watcher) =
        spawn_following(logs_command(repository.path(), config_home.path(), &["1"]));
    watcher.wait_for_banner();

    // The Worker is reconciled away mid-follow, so its log can never grow again.
    fs::remove_file(worker_state(repository.path(), "worker-1")).expect("remove worker state");

    let status = wait_bounded(&mut child, LIVENESS_TIMEOUT);
    assert_eq!(status.code(), Some(0), "a departed Worker is a clean end");

    let stderr = watcher.into_output();
    assert!(stderr.contains("left its Worker Pool"), "{stderr:?}");
}

#[test]
fn logs_returns_immediately_for_an_idle_worker() {
    let (repository, config_home) = jj_repository();
    write_manifest(repository.path(), &["worker-1"]);
    // An idle Worker has no Run, so the log its last Run left behind will never
    // grow again: `logs` prints the tail and returns rather than following it.
    write_worker(
        repository.path(),
        "worker-1",
        "idle",
        Some("previous run\n"),
    );

    let output = run_bounded(
        logs_command(repository.path(), config_home.path(), &["1"]),
        LIVENESS_TIMEOUT,
    );

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("previous run"),
        "the tail is still printed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    // There is nothing to follow, so the banner is not printed.
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("Following"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn logs_returns_when_a_running_worker_is_reset_to_idle() {
    let (repository, config_home) = jj_repository();
    write_manifest(repository.path(), &["worker-1"]);
    write_worker(repository.path(), "worker-1", "busy", Some("thinking\n"));

    let (mut child, watcher) =
        spawn_following(logs_command(repository.path(), config_home.path(), &["1"]));
    watcher.wait_for_banner();

    // A Reset abandons the Run and returns the Worker to idle, leaving the log
    // file in place. Nothing will append to it again, so the follow loop must
    // notice the Worker has no Run and return rather than spin on a stale log.
    write_idle_worker(repository.path(), "worker-1");

    let status = wait_bounded(&mut child, LIVENESS_TIMEOUT);
    assert_eq!(status.code(), Some(0), "an idle Worker is a clean end");

    let stderr = watcher.into_output();
    assert!(stderr.contains("the Worker is idle"), "{stderr:?}");
}

/// A `jjfx logs` invocation with `path` as `PATH`, so a fake `opencode` answers
/// the session API while the real `jj` is still found.
fn logs_command_with_path(
    repository: &Path,
    config_home: &Path,
    path: &std::ffi::OsStr,
    args: &[&str],
) -> Command {
    let mut command = logs_command(repository, config_home, args);
    command.env("PATH", path);
    command
}

#[test]
fn logs_shows_a_terminal_session_delivered_runs_activity() {
    let (repository, config_home) = jj_repository();
    let bin = TempDir::new().expect("fake opencode directory");
    write_manifest(repository.path(), &["worker-1"]);
    // A Run delivered through its opencode session writes no process log, so the
    // log is empty and the session is where its activity is.
    write_opencode_worker(repository.path(), "worker-1", "done", Some(""));
    write_worker_session(repository.path(), "worker-1", "ses_test");
    let path = fake_opencode(bin.path(), &session_messages());

    let output = run_bounded(
        logs_command_with_path(repository.path(), config_home.path(), &path, &["1"]),
        LIVENESS_TIMEOUT,
    );

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("reading the ticket"),
        "the session's activity is shown: {stdout:?}"
    );
    assert!(
        stdout.contains("tool bash Completed cargo test"),
        "the session's tool activity is shown: {stdout:?}"
    );
    // The prompt is not activity, and the empty log contributes nothing.
    assert!(!stdout.contains("Implement the ticket."), "{stdout:?}");
}

#[test]
fn logs_follows_a_running_session_delivered_run_and_ends_with_it() {
    let (repository, config_home) = jj_repository();
    let bin = TempDir::new().expect("fake opencode directory");
    write_manifest(repository.path(), &["worker-1"]);
    write_opencode_worker(repository.path(), "worker-1", "busy", Some(""));
    write_worker_session(repository.path(), "worker-1", "ses_test");
    let path = fake_opencode(bin.path(), &session_messages());

    let (mut child, watcher) = spawn_following(logs_command_with_path(
        repository.path(),
        config_home.path(),
        &path,
        &["1"],
    ));
    // Wait until follow mode is watching, so the Run genuinely ends under the
    // loop rather than before it attaches.
    watcher.wait_for_banner();

    // The Run ends. The follow loop must notice on its next pass and return
    // rather than follow a session that will never report again.
    write_opencode_worker(repository.path(), "worker-1", "done", Some(""));

    let status = wait_bounded(&mut child, LIVENESS_TIMEOUT);
    assert_eq!(status.code(), Some(0), "follow must return cleanly");

    let stderr = watcher.into_output();
    assert!(
        stderr.contains("Following opencode session ses_test for worker-1"),
        "the banner names the session it follows: {stderr:?}"
    );
    assert!(stderr.contains("Run is done"), "{stderr:?}");
}

#[test]
fn logs_says_plainly_when_a_session_delivered_runs_session_cannot_be_read() {
    let (repository, config_home) = jj_repository();
    let bin = TempDir::new().expect("fake opencode directory");
    write_manifest(repository.path(), &["worker-1"]);
    write_opencode_worker(repository.path(), "worker-1", "busy", Some(""));
    write_worker_session(repository.path(), "worker-1", "ses_test");
    let path = unreachable_opencode(bin.path());

    let output = run_bounded(
        logs_command_with_path(repository.path(), config_home.path(), &path, &["1"]),
        LIVENESS_TIMEOUT,
    );

    // There is no process log to show, and the session cannot be read, so the
    // command says so and names where the activity is rather than following an
    // empty file forever.
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).is_empty(),
        "there is no log to print: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No process log for worker-1"),
        "the gap is stated plainly: {stderr:?}"
    );
    assert!(
        stderr.contains("ses_test"),
        "the session to look at is named: {stderr:?}"
    );
}

#[test]
fn logs_follows_the_process_log_when_the_run_wrote_one() {
    let (repository, config_home) = jj_repository();
    let bin = TempDir::new().expect("fake opencode directory");
    write_manifest(repository.path(), &["worker-1"]);
    // An opencode Run whose process wrote a log keeps following that log, even
    // though its session is recorded: the log is the activity it actually wrote.
    write_opencode_worker(
        repository.path(),
        "worker-1",
        "busy",
        Some(r#"{"type":"text","sessionID":"ses_test","part":{"text":"from the process log"}}"#),
    );
    write_worker_session(repository.path(), "worker-1", "ses_test");
    let path = fake_opencode(bin.path(), &session_messages());

    let (mut child, watcher) = spawn_following(logs_command_with_path(
        repository.path(),
        config_home.path(),
        &path,
        &["1"],
    ));
    watcher.wait_for_banner();
    write_opencode_worker(
        repository.path(),
        "worker-1",
        "done",
        Some(r#"{"type":"text","sessionID":"ses_test","part":{"text":"from the process log"}}"#),
    );

    let status = wait_bounded(&mut child, LIVENESS_TIMEOUT);
    assert_eq!(status.code(), Some(0), "{status:?}");

    let stderr = watcher.into_output();
    assert!(
        stderr.contains("Following opencode log for worker-1"),
        "the process log is what is followed: {stderr:?}"
    );
    assert!(!stderr.contains("session ses_test"), "{stderr:?}");
}
