#![cfg(unix)]

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use rustix::fs::{flock, FlockOperation};
use rustix::process::{
    kill_process_group, test_kill_process, test_kill_process_group, Pid, Signal,
};
use tempfile::{tempdir, NamedTempFile};

const HELPER_MODE: &str = "WSG_UNIX_SPIKE_MODE";
const HELPER_LOCK_PATH: &str = "WSG_UNIX_SPIKE_LOCK_PATH";
const HELPER_RESULT_PATH: &str = "WSG_UNIX_SPIKE_RESULT_PATH";
const HELPER_TIMEOUT: Duration = Duration::from_secs(3);

struct ChildGuard {
    child: Option<Child>,
    process_group: Option<Pid>,
}

impl ChildGuard {
    fn spawn(command: &mut Command) -> Self {
        let child = command.spawn().expect("test helper should start");
        Self {
            child: Some(child),
            process_group: None,
        }
    }

    fn spawn_process_group(command: &mut Command) -> Self {
        command.process_group(0);
        let child = command.spawn().expect("test process group should start");
        let group = Pid::from_raw(i32::try_from(child.id()).expect("group ID should fit in i32"))
            .expect("group ID should be non-zero");
        Self {
            child: Some(child),
            process_group: Some(group),
        }
    }

    fn id(&self) -> u32 {
        self.child.as_ref().expect("child should be present").id()
    }

    fn take_stdout(&mut self) -> ChildStdout {
        self.child
            .as_mut()
            .expect("child should be present")
            .stdout
            .take()
            .expect("child stdout should be piped")
    }

    fn wait(&mut self, timeout: Duration) -> ExitStatus {
        self.try_wait_for(timeout).expect("test helper timed out")
    }

    fn try_wait_for(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            let child = self.child.as_mut().expect("child should be present");
            if let Some(status) = child.try_wait().expect("helper status should be readable") {
                self.child = None;
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn is_reaped(&self) -> bool {
        self.child.is_none()
    }

    fn disarm_process_group(&mut self) {
        self.process_group = None;
    }

    fn kill_and_wait(&mut self) {
        if let Some(group) = self.process_group.take() {
            let _ = kill_process_group(group, Signal::KILL);
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill_and_wait();
    }
}

fn pid_is_live(pid: Pid) -> bool {
    test_kill_process(pid).is_ok()
}

fn process_group_is_live(group: Pid) -> bool {
    test_kill_process_group(group).is_ok()
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    condition()
}

fn terminate_process_group(child: &mut ChildGuard, group: Pid, grace: Duration) {
    kill_process_group(group, Signal::TERM).expect("graceful group signal should succeed");
    let _ = child.try_wait_for(grace);
    if process_group_is_live(group) {
        kill_process_group(group, Signal::KILL).expect("forced group signal should succeed");
    }
    if !child.is_reaped() {
        assert!(
            child.try_wait_for(grace).is_some(),
            "forced group termination timed out"
        );
    }
}

fn replace_state_file(target: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "target has no parent"))?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(contents)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary
        .into_temp_path()
        .persist(target)
        .map(|_| ())
        .map_err(|error| error.error)
}

fn spawn_logged_child(log_path: &Path) -> ChildGuard {
    let log = File::create(log_path).expect("log file should be created");
    let error_log = log
        .try_clone()
        .expect("log handle should be cloned for stderr");
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg("printf 'first\\n'; sleep 0.05; printf 'second\\n'")
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(error_log));
    ChildGuard::spawn(&mut command)
}

fn helper_command(mode: &str, lock_path: &Path, result_path: &Path) -> Command {
    let mut command = Command::new(env::current_exe().expect("test executable should have a path"));
    command
        .arg("--exact")
        .arg("unix_lock_helper")
        .arg("--ignored")
        .env(HELPER_MODE, mode)
        .env(HELPER_LOCK_PATH, lock_path)
        .env(HELPER_RESULT_PATH, result_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn read_line_with_timeout(stdout: ChildStdout, timeout: Duration) -> String {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        let _ = sender.send(result);
    });
    receiver
        .recv_timeout(timeout)
        .expect("timed out reading helper output")
        .expect("helper output should be readable")
}

fn wait_for_file(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn path_from_env(name: &str) -> PathBuf {
    env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("{name} should be set"))
}

#[test]
#[ignore]
fn unix_lock_helper() {
    let mode = env::var(HELPER_MODE).expect("helper mode should be set");
    let lock_path = path_from_env(HELPER_LOCK_PATH);
    let result_path = path_from_env(HELPER_RESULT_PATH);
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .expect("lock sidecar should open");

    match mode.as_str() {
        "hold" => {
            flock(&lock_file, FlockOperation::LockExclusive)
                .expect("exclusive lock should succeed");
            fs::write(result_path, "locked").expect("holder should report readiness");
            thread::sleep(Duration::from_secs(30));
        }
        "try" => {
            let result = match flock(&lock_file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => "acquired",
                Err(error) if error == rustix::io::Errno::WOULDBLOCK => "blocked",
                Err(error) => panic!("unexpected lock error: {error}"),
            };
            fs::write(result_path, result).expect("contender should report its result");
        }
        other => panic!("unknown helper mode: {other}"),
    }
}

#[test]
fn exclusive_sidecar_lock_serializes_independent_processes() {
    let temp = tempdir().expect("temporary directory should be created");

    for (index, lock_name) in [".dispatch.lock", "worker-abc123.json.lock"]
        .into_iter()
        .enumerate()
    {
        let lock_path = temp.path().join(lock_name);
        let holder_result = temp.path().join(format!("holder-result-{index}"));
        let blocked_result = temp.path().join(format!("blocked-result-{index}"));
        let acquired_result = temp.path().join(format!("acquired-result-{index}"));

        let mut holder = ChildGuard::spawn(&mut helper_command("hold", &lock_path, &holder_result));
        wait_for_file(&holder_result, HELPER_TIMEOUT);

        let mut blocked =
            ChildGuard::spawn(&mut helper_command("try", &lock_path, &blocked_result));
        assert!(blocked.wait(HELPER_TIMEOUT).success());
        assert_eq!(
            fs::read_to_string(&blocked_result).expect("contender result should exist"),
            "blocked"
        );

        holder.kill_and_wait();

        let mut acquired =
            ChildGuard::spawn(&mut helper_command("try", &lock_path, &acquired_result));
        assert!(acquired.wait(HELPER_TIMEOUT).success());
        assert_eq!(
            fs::read_to_string(&acquired_result).expect("contender result should exist"),
            "acquired"
        );
    }
}

#[test]
fn spawned_command_leads_its_own_process_group() {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg("ps -o pgid= -p $$ | tr -d ' '; sleep 30")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = ChildGuard::spawn_process_group(&mut command);
    let pid = child.id();
    let reported_group: u32 = read_line_with_timeout(child.take_stdout(), HELPER_TIMEOUT)
        .trim()
        .parse()
        .expect("process group should be numeric");

    assert_eq!(reported_group, pid);
}

#[test]
fn pid_liveness_changes_after_the_child_is_reaped() {
    let mut command = Command::new("sh");
    command.arg("-c").arg("sleep 30").stderr(Stdio::null());
    let mut child = ChildGuard::spawn(&mut command);
    let pid = Pid::from_raw(i32::try_from(child.id()).expect("child PID should fit in i32"))
        .expect("child PID should be non-zero");

    assert!(pid_is_live(pid));
    child.kill_and_wait();
    assert!(!pid_is_live(pid));
}

#[test]
fn group_termination_is_graceful_then_forced_and_removes_descendants() {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg("trap '' TERM; sh -c 'trap \\\"\\\" TERM; while :; do :; done' & child=$!; printf '%s\\n' \"$child\"; wait \"$child\"; while :; do :; done")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut leader = ChildGuard::spawn_process_group(&mut command);
    let group = Pid::from_raw(i32::try_from(leader.id()).expect("group ID should fit in i32"))
        .expect("group ID should be non-zero");
    let descendant_raw: i32 = read_line_with_timeout(leader.take_stdout(), HELPER_TIMEOUT)
        .trim()
        .parse()
        .expect("descendant PID should be numeric");
    let descendant = Pid::from_raw(descendant_raw).expect("descendant PID should be non-zero");

    assert!(pid_is_live(descendant), "descendant should start alive");
    terminate_process_group(&mut leader, group, Duration::from_millis(200));

    assert!(
        wait_until(HELPER_TIMEOUT, || !pid_is_live(descendant)),
        "forced signal should remove the stubborn descendant"
    );
    assert!(
        leader.is_reaped(),
        "forced signal should remove and reap the stubborn leader"
    );
    assert!(
        wait_until(HELPER_TIMEOUT, || !process_group_is_live(group)),
        "terminated process group should disappear"
    );
    leader.disarm_process_group();
}

#[test]
fn child_keeps_writing_after_the_parent_closes_its_log_handle() {
    let temp = tempdir().expect("temporary directory should be created");
    let log_path = temp.path().join("worker.log");
    let mut child = spawn_logged_child(&log_path);

    assert!(child.wait(HELPER_TIMEOUT).success());
    assert_eq!(
        fs::read_to_string(log_path).expect("log should be readable"),
        "first\nsecond\n"
    );
}

#[test]
fn same_directory_replacement_is_atomic_for_concurrent_readers() {
    let temp = tempdir().expect("temporary directory should be created");
    let target = temp.path().join("pool.json");
    let old = vec![b'o'; 1024 * 1024];
    let new = vec![b'n'; 1024 * 1024];
    fs::write(&target, &old).expect("initial state should be written");

    let reading = Arc::new(AtomicBool::new(true));
    let saw_partial = Arc::new(AtomicBool::new(false));
    let saw_new = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicUsize::new(0));
    let (started_sender, started_receiver) = mpsc::channel();
    let reader_target = target.clone();
    let reader_old = old.clone();
    let reader_new = new.clone();
    let reader_running = Arc::clone(&reading);
    let reader_partial = Arc::clone(&saw_partial);
    let reader_saw_new = Arc::clone(&saw_new);
    let reader_reads = Arc::clone(&reads);
    let reader = thread::spawn(move || {
        let deadline = Instant::now() + HELPER_TIMEOUT;
        let mut started_sender = Some(started_sender);
        while reader_running.load(Ordering::Acquire) && Instant::now() < deadline {
            match fs::read(&reader_target) {
                Ok(bytes) if bytes == reader_old => {}
                Ok(bytes) if bytes == reader_new => reader_saw_new.store(true, Ordering::Release),
                Ok(_) | Err(_) => reader_partial.store(true, Ordering::Release),
            }
            reader_reads.fetch_add(1, Ordering::AcqRel);
            if let Some(sender) = started_sender.take() {
                let _ = sender.send(());
            }
        }
    });

    started_receiver
        .recv_timeout(HELPER_TIMEOUT)
        .expect("reader should observe the old state before replacement");
    replace_state_file(&target, &new).expect("state replacement should succeed");
    assert!(
        wait_until(HELPER_TIMEOUT, || saw_new.load(Ordering::Acquire)),
        "reader should observe the new state after replacement"
    );
    reading.store(false, Ordering::Release);
    reader.join().expect("reader should finish");

    assert!(
        reads.load(Ordering::Acquire) >= 2,
        "reader should run across replacement"
    );
    assert!(
        !saw_partial.load(Ordering::Acquire),
        "reader observed a partial replacement"
    );
    assert_eq!(
        fs::read(target).expect("replacement should be readable"),
        new
    );
}
