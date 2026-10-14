#![cfg(unix)]

use std::env;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use jjfx::engine::{
    CommitOutcome, DispatchGroupOptions, DispatchGroupState, Expected, Loaded, PoolState,
    Repository, StateChange, TicketId, WireStatus, WireTimestamp, WorkerId, WorkerState,
};
use rustix::fs::{FlockOperation, flock};
use tempfile::TempDir;

mod support;

use support::HELPER_LIVENESS_TIMEOUT;

const MODE: &str = "WSG_RUST_STATE_HELPER_MODE";
const LOCK: &str = "WSG_RUST_STATE_HELPER_LOCK";
const READY: &str = "WSG_RUST_STATE_HELPER_READY";
const RELEASE: &str = "WSG_RUST_STATE_HELPER_RELEASE";
const RESULT: &str = "WSG_RUST_STATE_HELPER_RESULT";
const ROOT: &str = "WSG_RUST_STATE_HELPER_ROOT";
const KIND: &str = "WSG_RUST_STATE_HELPER_KIND";
const VALUE: &str = "WSG_RUST_STATE_HELPER_VALUE";

struct ChildGuard(Option<Child>);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
#[ignore]
fn rust_state_lock_helper() {
    match env::var(MODE).as_deref() {
        Ok("hold") => hold_lock_for_helper(),
        Ok("cas") => commit_for_helper(),
        _ => {}
    }
}

fn hold_lock_for_helper() {
    let lock = PathBuf::from(env::var_os(LOCK).expect("lock path"));
    let ready = PathBuf::from(env::var_os(READY).expect("ready path"));
    fs::create_dir_all(lock.parent().expect("lock parent")).expect("lock directory");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock)
        .expect("lock file");
    flock(&file, FlockOperation::LockExclusive).expect("lock");
    fs::write(ready, b"ready").expect("ready marker");
    thread::sleep(Duration::from_secs(30));
}

fn commit_for_helper() {
    let root = PathBuf::from(env::var_os(ROOT).expect("repository root"));
    let ready = PathBuf::from(env::var_os(READY).expect("ready path"));
    let release = PathBuf::from(env::var_os(RELEASE).expect("release path"));
    let result = PathBuf::from(env::var_os(RESULT).expect("result path"));
    let kind = env::var(KIND).expect("state kind");
    let value = env::var(VALUE).expect("mutation value");
    let repository = Repository::open(root).expect("repository");

    let outcome = match kind.as_str() {
        "pool" => {
            let state = repository.state_store().pool();
            let Loaded::Present(versioned) = state.load().expect("load Pool") else {
                panic!("Pool missing")
            };
            let (mut current, revision) = versioned.into_parts();
            current.gh_repo = value;
            fs::write(&ready, b"ready").expect("ready marker");
            wait_for_helper_release(&release);
            outcome_name(
                state
                    .commit(Expected::Match(revision), StateChange::Replace(current))
                    .expect("commit Pool"),
            )
        }
        "worker" => {
            let state = repository
                .state_store()
                .worker(WorkerId::parse("worker-01").expect("Worker ID"));
            let Loaded::Present(versioned) = state.load().expect("load Worker") else {
                panic!("Worker missing")
            };
            let (mut current, revision) = versioned.into_parts();
            current.ticket = Some(value);
            fs::write(&ready, b"ready").expect("ready marker");
            wait_for_helper_release(&release);
            outcome_name(
                state
                    .commit(Expected::Match(revision), StateChange::Replace(current))
                    .expect("commit Worker"),
            )
        }
        "dispatch" => {
            let state = repository
                .state_store()
                .dispatch_group(TicketId::parse("ENG-100").expect("Ticket ID"));
            let Loaded::Present(versioned) = state.load().expect("load Dispatch Group") else {
                panic!("Dispatch Group missing")
            };
            let (mut current, revision) = versioned.into_parts();
            current.gh_repo = value;
            fs::write(&ready, b"ready").expect("ready marker");
            wait_for_helper_release(&release);
            outcome_name(
                state
                    .commit(Expected::Match(revision), StateChange::Replace(current))
                    .expect("commit Dispatch Group"),
            )
        }
        other => panic!("unknown state kind {other}"),
    };
    fs::write(result, outcome).expect("result marker");
}

fn outcome_name<T>(outcome: CommitOutcome<T>) -> &'static str {
    match outcome {
        CommitOutcome::Applied(_) => "applied",
        CommitOutcome::Conflict(_) => "conflict",
    }
}

fn wait_for_helper_release(path: &Path) {
    let deadline = Instant::now() + HELPER_LIVENESS_TIMEOUT;
    while !path.exists() {
        assert!(Instant::now() < deadline, "timed out waiting for release");
        thread::sleep(Duration::from_millis(10));
    }
}

fn repository() -> (TempDir, Repository) {
    let temp = tempfile::tempdir().expect("temp repository");
    fs::create_dir(temp.path().join(".jj")).expect("repository marker");
    let repository = Repository::open(temp.path()).expect("repository");
    (temp, repository)
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + HELPER_LIVENESS_TIMEOUT;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_child(child: &mut ChildGuard) {
    let deadline = Instant::now() + HELPER_LIVENESS_TIMEOUT;
    loop {
        let process = child.0.as_mut().expect("child process");
        if let Some(status) = process.try_wait().expect("child status") {
            assert!(status.success(), "helper failed: {status}");
            child.0 = None;
            return;
        }
        assert!(Instant::now() < deadline, "helper timed out");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn independent_rust_repository_writers_detect_lost_updates() {
    for kind in ["pool", "worker", "dispatch"] {
        let (temp, repository) = repository();
        match kind {
            "pool" => {
                repository
                    .state_store()
                    .pool()
                    .commit(
                        Expected::Missing,
                        StateChange::Replace(PoolState::new(
                            0,
                            "Jarvvski/jjfx",
                            Vec::new(),
                            WireTimestamp::new("2026-07-27T10:00:00Z"),
                        )),
                    )
                    .expect("seed Pool");
            }
            "worker" => {
                repository
                    .state_store()
                    .worker(WorkerId::parse("worker-01").expect("Worker ID"))
                    .commit(
                        Expected::Missing,
                        StateChange::Replace(WorkerState::new(WireStatus::new("idle"))),
                    )
                    .expect("seed Worker");
            }
            "dispatch" => {
                let parent = TicketId::parse("ENG-100").expect("Ticket ID");
                repository
                    .state_store()
                    .dispatch_group(parent.clone())
                    .commit(
                        Expected::Missing,
                        StateChange::Replace(DispatchGroupState::new(
                            parent,
                            WireTimestamp::new("2026-07-27T10:00:00Z"),
                            "Jarvvski/jjfx",
                            DispatchGroupOptions::new(""),
                        )),
                    )
                    .expect("seed Dispatch Group");
            }
            _ => unreachable!(),
        }

        let release = temp.path().join(format!("{kind}-release"));
        let mut children = Vec::new();
        let mut results = Vec::new();
        for index in 0..2 {
            let ready = temp.path().join(format!("{kind}-ready-{index}"));
            let result = temp.path().join(format!("{kind}-result-{index}"));
            let mut command = Command::new(env::current_exe().expect("test executable"));
            command
                .arg("--exact")
                .arg("rust_state_lock_helper")
                .arg("--ignored")
                .env(MODE, "cas")
                .env(ROOT, temp.path())
                .env(KIND, kind)
                .env(VALUE, format!("writer-{index}"))
                .env(READY, &ready)
                .env(RELEASE, &release)
                .env(RESULT, &result)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            children.push(ChildGuard(Some(command.spawn().expect("CAS helper"))));
            results.push((ready, result));
        }
        for (ready, _) in &results {
            wait_for(ready);
        }
        fs::write(&release, b"release").expect("release helpers");
        for child in &mut children {
            wait_for_child(child);
        }
        let mut outcomes: Vec<_> = results
            .iter()
            .map(|(_, result)| fs::read_to_string(result).expect("helper result"))
            .collect();
        outcomes.sort();
        assert_eq!(outcomes, ["applied", "conflict"], "state kind {kind}");
    }
}

#[test]
fn an_independent_rust_process_serializes_pool_commits() {
    let (temp, repository) = repository();
    let lock = temp.path().join(".jj/pool/.dispatch.lock");
    let ready = temp.path().join("ready");
    let mut command = Command::new(env::current_exe().expect("test executable"));
    command
        .arg("--exact")
        .arg("rust_state_lock_helper")
        .arg("--ignored")
        .env(MODE, "hold")
        .env(LOCK, &lock)
        .env(READY, &ready)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut holder = ChildGuard(Some(command.spawn().expect("holder")));
    wait_for(&ready);

    let pool = repository.state_store().pool();
    let state = PoolState::new(
        0,
        "Jarvvski/jjfx",
        Vec::new(),
        WireTimestamp::new("2026-07-27T10:00:00Z"),
    );
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(pool.commit(Expected::Missing, StateChange::Replace(state)));
    });
    assert!(
        receiver.recv_timeout(Duration::from_millis(100)).is_err(),
        "commit bypassed held lock"
    );
    if let Some(mut child) = holder.0.take() {
        child.kill().expect("kill holder");
        child.wait().expect("wait holder");
    }
    assert!(
        receiver
            .recv_timeout(HELPER_LIVENESS_TIMEOUT)
            .expect("commit completed")
            .is_ok()
    );
}

#[test]
fn an_independent_rust_process_serializes_worker_commits() {
    let (temp, repository) = repository();
    let lock = temp.path().join(".jj/pool/worker-01.json.lock");
    let ready = temp.path().join("worker-ready");
    let mut command = Command::new(env::current_exe().expect("test executable"));
    command
        .arg("--exact")
        .arg("rust_state_lock_helper")
        .arg("--ignored")
        .env(MODE, "hold")
        .env(LOCK, &lock)
        .env(READY, &ready)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut holder = ChildGuard(Some(command.spawn().expect("holder")));
    wait_for(&ready);
    let worker = repository
        .state_store()
        .worker(WorkerId::parse("worker-01").expect("Worker ID"));
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(worker.commit(
            Expected::Missing,
            StateChange::Replace(WorkerState::new(WireStatus::new("idle"))),
        ));
    });
    assert!(
        receiver.recv_timeout(Duration::from_millis(100)).is_err(),
        "commit bypassed held lock"
    );
    if let Some(mut child) = holder.0.take() {
        child.kill().expect("kill holder");
        child.wait().expect("wait holder");
    }
    assert!(
        receiver
            .recv_timeout(HELPER_LIVENESS_TIMEOUT)
            .expect("commit completed")
            .is_ok()
    );
}

#[test]
fn an_independent_rust_process_serializes_dispatch_group_commits() {
    let (temp, repository) = repository();
    let lock = temp.path().join(".jj/pool/dispatch-eng-100.json.lock");
    let ready = temp.path().join("group-ready");
    let mut command = Command::new(env::current_exe().expect("test executable"));
    command
        .arg("--exact")
        .arg("rust_state_lock_helper")
        .arg("--ignored")
        .env(MODE, "hold")
        .env(LOCK, &lock)
        .env(READY, &ready)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut holder = ChildGuard(Some(command.spawn().expect("holder")));
    wait_for(&ready);
    let parent = TicketId::parse("ENG-100").expect("Ticket ID");
    let state = DispatchGroupState::new(
        parent.clone(),
        WireTimestamp::new("2026-07-27T10:00:00Z"),
        "Jarvvski/jjfx",
        DispatchGroupOptions::new(""),
    );
    let group = repository.state_store().dispatch_group(parent);
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(group.commit(Expected::Missing, StateChange::Replace(state)));
    });
    assert!(
        receiver.recv_timeout(Duration::from_millis(100)).is_err(),
        "commit bypassed held lock"
    );
    if let Some(mut child) = holder.0.take() {
        child.kill().expect("kill holder");
        child.wait().expect("wait holder");
    }
    assert!(
        receiver
            .recv_timeout(HELPER_LIVENESS_TIMEOUT)
            .expect("commit completed")
            .is_ok()
    );
}
