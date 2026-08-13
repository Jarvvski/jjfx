use std::env;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rustix::fs::{FlockOperation, flock};
use rustix::process::{Pid, Signal, kill_process_group, test_kill_process};
use serde_json::Value;
use tempfile::TempDir;
use wsg_core::{DispatchGroup, Expected, Loaded, Repository, StateChange, TicketId, WireStatus};

const LOCK_HOLDER_MODE: &str = "WSG_CONFORMANCE_LOCK_HOLDER_MODE";
const LOCK_HOLDER_PATH: &str = "WSG_CONFORMANCE_LOCK_HOLDER_PATH";
const LOCK_HOLDER_READY: &str = "WSG_CONFORMANCE_LOCK_HOLDER_READY";
const LOCK_HOLDER_RELEASE: &str = "WSG_CONFORMANCE_LOCK_HOLDER_RELEASE";

use anyhow::{Result, bail};

#[derive(Debug, Clone)]
pub(crate) struct BinarySpec {
    label: &'static str,
    executable: PathBuf,
}

pub(crate) fn local_repository() -> TempDir {
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let output = Command::new("jj")
        .args(["--config", "signing.behavior=drop", "git", "init"])
        .arg(directory.path())
        .output()
        .expect("jj should run");
    assert!(
        output.status.success(),
        "jj init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let remote = Command::new("jj")
        .args(["git", "remote", "add", "origin", "owner/repo"])
        .current_dir(directory.path())
        .output()
        .expect("jj remote add should run");
    assert!(
        remote.status.success(),
        "jj remote add failed: {}",
        String::from_utf8_lossy(&remote.stderr)
    );

    let bookmark = Command::new("jj")
        .args(["bookmark", "create", "main"])
        .current_dir(directory.path())
        .output()
        .expect("jj bookmark create should run");
    assert!(
        bookmark.status.success(),
        "jj bookmark create failed: {}",
        String::from_utf8_lossy(&bookmark.stderr)
    );
    directory
}

pub(crate) struct LockBarrier {
    child: Option<Child>,
    release: PathBuf,
}

impl LockBarrier {
    pub(crate) fn acquire(root: &Path) -> Self {
        let lock = root.join(".jj/pool/.dispatch.lock");
        let ready = root.join("conformance-lock-ready");
        let release = root.join("conformance-lock-release");
        let child = Command::new(env::current_exe().expect("test executable"))
            .args(["--exact", "conformance_lock_helper", "--ignored"])
            .env(LOCK_HOLDER_MODE, "hold")
            .env(LOCK_HOLDER_PATH, &lock)
            .env(LOCK_HOLDER_READY, &ready)
            .env(LOCK_HOLDER_RELEASE, &release)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("lock helper should spawn");
        wait_for(&ready);
        Self {
            child: Some(child),
            release,
        }
    }

    pub(crate) fn release(mut self) {
        fs::write(&self.release, b"release").expect("lock release marker should be written");
        wait_for_child(self.child.take().expect("lock helper child"));
    }
}

impl Drop for LockBarrier {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub(crate) fn run_lock_helper() {
    if env::var(LOCK_HOLDER_MODE).as_deref() != Ok("hold") {
        return;
    }
    let lock = PathBuf::from(env::var_os(LOCK_HOLDER_PATH).expect("lock path"));
    let ready = PathBuf::from(env::var_os(LOCK_HOLDER_READY).expect("ready path"));
    let release = PathBuf::from(env::var_os(LOCK_HOLDER_RELEASE).expect("release path"));
    fs::create_dir_all(lock.parent().expect("lock parent")).expect("lock directory");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock)
        .expect("lock file");
    flock(&file, FlockOperation::LockExclusive).expect("exclusive lock");
    fs::write(ready, b"ready").expect("ready marker");
    wait_for(&release);
}

pub(crate) fn wait_for_file(path: &Path) {
    wait_for(path);
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_child(mut child: Child) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().expect("lock helper status") {
            assert!(status.success(), "lock helper failed: {status}");
            return;
        }
        assert!(Instant::now() < deadline, "lock helper timed out");
        thread::sleep(Duration::from_millis(10));
    }
}

pub(crate) fn add_unknown_field(path: &Path, field: &str) {
    let mut document: Value = serde_json::from_slice(&fs::read(path).expect("state should read"))
        .expect("state should be valid JSON");
    document
        .as_object_mut()
        .expect("state should be a JSON object")
        .insert(field.to_owned(), Value::Bool(true));
    let mut bytes = serde_json::to_vec_pretty(&document).expect("state should serialize");
    bytes.push(b'\n');
    fs::write(path, bytes).expect("state should be rewritten");
}

pub(crate) fn interrupted_artifact(path: &Path) -> Child {
    let child = Command::new("sh")
        .args(["-c", "printf '%s' '{' > \"$1\"; sleep 30", "writer"])
        .arg(path)
        .spawn()
        .expect("interrupted writer should spawn");
    wait_for_file(path);
    child
}

pub(crate) fn stop_child(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

pub(crate) fn wait_for_assigned_group_worker(root: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    let parent = TicketId::parse("ENG-100").expect("Parent Ticket should parse");
    loop {
        let repository = Repository::open(root).expect("repository should open");
        if let Loaded::Present(group) = repository
            .state_store()
            .dispatch_group(parent.clone())
            .load()
            .expect("Dispatch Group should load")
            && let Some(worker) = group
                .value
                .sub_issues
                .values()
                .find_map(|issue| issue.worker.clone())
        {
            return worker.to_string();
        }
        assert!(
            Instant::now() < deadline,
            "no Dispatch Group assignment appeared"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

pub(crate) fn dispatch_group_counts(root: &Path) -> (usize, usize, usize, usize) {
    let repository = Repository::open(root).expect("repository should open");
    let parent = TicketId::parse("ENG-100").expect("Parent Ticket should parse");
    let Loaded::Present(state) = repository
        .state_store()
        .dispatch_group(parent)
        .load()
        .expect("Dispatch Group should load")
    else {
        panic!("Dispatch Group should exist");
    };
    let total = state.value.sub_issues.len();
    let group = DispatchGroup::from_state(state.value).expect("Dispatch Group should validate");
    let counts = group.status_counts();
    (counts.done(), counts.failed(), counts.skipped(), total)
}

pub(crate) fn first_worker(root: &Path) -> String {
    let repository = Repository::open(root).expect("repository should open");
    let Loaded::Present(pool) = repository
        .state_store()
        .pool()
        .load()
        .expect("Pool should load")
    else {
        panic!("Pool should exist");
    };
    pool.value
        .workers
        .first()
        .expect("Pool should have one Worker")
        .to_string()
}

pub(crate) fn mark_worker_done(root: &Path, worker: &str, ticket: &str) {
    let repository = Repository::open(root).expect("repository should open");
    let worker = wsg_core::WorkerId::parse(worker).expect("Worker ID should be valid");
    let state = repository.state_store().worker(worker);
    let Loaded::Present(versioned) = state.load().expect("Worker should load") else {
        panic!("Worker should exist");
    };
    let (mut worker_state, revision) = versioned.into_parts();
    worker_state.status = WireStatus::new("done");
    worker_state.ticket = Some(ticket.to_owned());
    worker_state.pid = None;
    worker_state.exit_code = Some(0);
    worker_state.completed_at = Some(wsg_core::WireTimestamp::new("2026-08-13T00:00:00Z"));
    state
        .commit(
            Expected::Match(revision),
            StateChange::Replace(worker_state),
        )
        .expect("done Worker fixture should commit");
}

pub(crate) fn recorded_worker_pid(root: &Path, worker: &str) -> u32 {
    let repository = Repository::open(root).expect("repository should open");
    let worker = wsg_core::WorkerId::parse(worker).expect("Worker ID should be valid");
    let Loaded::Present(state) = repository
        .state_store()
        .worker(worker)
        .load()
        .expect("Worker should load")
    else {
        panic!("Worker should exist");
    };
    state
        .value
        .pid
        .and_then(|pid| u32::try_from(pid).ok())
        .expect("Worker should record a process PID")
}

pub(crate) struct ProcessTreeGuard {
    leader: Option<Pid>,
}

impl ProcessTreeGuard {
    pub(crate) fn new(pid: u32) -> Self {
        Self {
            leader: pid_from_u32(pid),
        }
    }
}

impl Drop for ProcessTreeGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.leader.take() {
            let _ = kill_process_group(pid, Signal::KILL);
        }
    }
}

pub(crate) fn wait_for_process_exit(pid: u32) {
    let pid = pid_from_u32(pid).expect("process PID should be positive");
    let deadline = Instant::now() + Duration::from_secs(5);
    while test_kill_process(pid).is_ok() {
        assert!(Instant::now() < deadline, "process {pid:?} did not exit");
        thread::sleep(Duration::from_millis(20));
    }
}

fn pid_from_u32(pid: u32) -> Option<Pid> {
    Pid::from_raw(i32::try_from(pid).expect("process PID should fit in i32"))
}

pub(crate) fn mark_worker_busy(root: &Path) -> String {
    let repository = Repository::open(root).expect("repository should open");
    let Loaded::Present(pool) = repository
        .state_store()
        .pool()
        .load()
        .expect("Pool should load")
    else {
        panic!("Pool should exist");
    };
    let worker = pool
        .value
        .workers
        .first()
        .cloned()
        .expect("Pool should have one Worker");
    let state = repository.state_store().worker(worker.clone());
    let Loaded::Present(versioned) = state.load().expect("Worker should load") else {
        panic!("Worker should exist");
    };
    let (mut worker_state, revision) = versioned.into_parts();
    worker_state.status = WireStatus::new("busy");
    worker_state.ticket = Some("ENG-CONFORMANCE".to_owned());
    worker_state.pid = Some(999_999_999);
    state
        .commit(
            Expected::Match(revision),
            StateChange::Replace(worker_state),
        )
        .expect("busy Worker fixture should commit");
    worker.to_string()
}

pub(crate) fn wait_with_output(mut child: Child, label: &str) -> CommandOutcome {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if child
            .try_wait()
            .unwrap_or_else(|error| panic!("{label} status failed: {error}"))
            .is_some()
        {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{label} exceeded the 30 second conformance timeout");
        }
        thread::sleep(Duration::from_millis(10));
    }
    child
        .wait_with_output()
        .unwrap_or_else(|error| panic!("{label} output failed: {error}"))
        .into()
}

impl BinarySpec {
    pub(crate) fn new(label: &'static str, executable: PathBuf) -> Self {
        Self { label, executable }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.executable
    }

    pub(crate) fn run(&self, directory: &Path, args: &[&str]) -> CommandOutcome {
        self.run_with_environment(directory, args, &[])
    }

    pub(crate) fn run_with_input_and_environment(
        &self,
        directory: &Path,
        args: &[&str],
        input: &[u8],
        environment: &[(&str, &OsStr)],
    ) -> CommandOutcome {
        let mut command = Command::new(&self.executable);
        command
            .args(args)
            .current_dir(directory)
            .envs(environment.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap_or_else(|error| {
            panic!(
                "{} binary {} should run: {error}",
                self.label,
                self.executable.display()
            )
        });
        child
            .stdin
            .take()
            .expect("input-enabled conformance process should have stdin")
            .write_all(input)
            .unwrap_or_else(|error| panic!("{label} input failed: {error}", label = self.label));
        wait_with_output(child, self.label)
    }

    pub(crate) fn run_with_environment(
        &self,
        directory: &Path,
        args: &[&str],
        environment: &[(&str, &OsStr)],
    ) -> CommandOutcome {
        let mut command = Command::new(&self.executable);
        command
            .args(args)
            .current_dir(directory)
            .envs(environment.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn().unwrap_or_else(|error| {
            panic!(
                "{} binary {} should run: {error}",
                self.label,
                self.executable.display()
            )
        });
        wait_with_output(child, self.label)
    }
}

#[derive(Debug)]
pub(crate) struct CommandOutcome {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

impl From<std::process::Output> for CommandOutcome {
    fn from(output: std::process::Output) -> Self {
        Self {
            status: output.status,
            stdout: output.stdout,
            stderr: output.stderr,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ConformanceBinaries {
    pub(crate) rust: BinarySpec,
    pub(crate) go: BinarySpec,
    pub(crate) go_test: BinarySpec,
}

impl ConformanceBinaries {
    pub(crate) fn from_environment() -> Result<Self> {
        Self::from_explicit(
            PathBuf::from(env!("CARGO_BIN_EXE_wsg")),
            std::env::var_os("WSG_GO_BINARY").map(PathBuf::from),
            std::env::var_os("WSG_GO_TEST_BINARY").map(PathBuf::from),
        )
    }

    pub(crate) fn from_explicit(
        rust: PathBuf,
        go: Option<PathBuf>,
        go_test: Option<PathBuf>,
    ) -> Result<Self> {
        let mut missing = Vec::new();
        validate_path(&rust, "CARGO_BIN_EXE_wsg", &mut missing);

        let go = go.map(|path| {
            validate_path(&path, "WSG_GO_BINARY", &mut missing);
            BinarySpec::new("Go wsg", path)
        });
        let go_test = go_test.map(|path| {
            validate_path(&path, "WSG_GO_TEST_BINARY", &mut missing);
            BinarySpec::new("Go wsg test helper", path)
        });

        if go.is_none() {
            missing.push("WSG_GO_BINARY".to_owned());
        }
        if go_test.is_none() {
            missing.push("WSG_GO_TEST_BINARY".to_owned());
        }
        if !missing.is_empty() {
            bail!(
                "Go/Rust conformance requires explicit executable paths: {}",
                missing.join(", ")
            );
        }

        Ok(Self {
            rust: BinarySpec::new("Rust wsg", rust),
            go: go.expect("Go path was checked above"),
            go_test: go_test.expect("Go test path was checked above"),
        })
    }
}

fn validate_path(path: &Path, variable: &str, missing: &mut Vec<String>) {
    if !path.is_file() {
        missing.push(format!("{variable} ({})", path.display()));
    }
}
