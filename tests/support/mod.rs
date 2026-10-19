//! Values shared by the integration tests.
//!
//! Each file directly under `tests/` is its own crate, so anything shared has to
//! live in a module like this one and be pulled in with `mod support;`.
//!
//! Because this module is compiled into every test crate, a helper used by only
//! some of them is dead code in the rest. That is the point of a shared toolbox,
//! so dead-code analysis is off here rather than worked around per crate.
#![allow(dead_code)]

#[cfg(unix)]
use std::ffi::OsString;
use std::fs;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process_group};

/// Writes a fake `kitten` into `bin` that answers `kitten @ ls` with an empty
/// workspace list, and returns a `PATH` with `bin` ahead of the inherited one.
///
/// `dispatch` fails closed when it cannot see which Worker Workspaces are open
/// (ADR 0011): it names `kitten @ ls` and suggests `--force`. That is the right
/// product behaviour, but it makes every Dispatch test that runs the real
/// terminal seam depend on the machine it runs on - green on a developer box
/// running kitty, red on CI, which is exactly how a ticket-05 test left `main`
/// red. This stub answers the seam deterministically, so a Dispatch test needs
/// neither kitty nor `--force`, and it exercises the real seam rather than
/// bypassing it. Use it in every fixture that runs `jjfx dispatch`.
///
/// `bin` must outlive the returned `PATH` (hold it in a `TempDir`), and the
/// command under test must be given the returned `PATH`.
#[cfg(unix)]
pub fn stub_terminal(bin: &Path) -> OsString {
    let kitten = bin.join("kitten");
    // An empty array is a `kitten @ ls` tree with no tabs open, so no Workspace
    // is reported as occupied and Dispatch proceeds without `--force`.
    fs::write(&kitten, "#!/bin/sh\nprintf '[]\\n'\n").expect("terminal stub");
    let mut permissions = fs::metadata(&kitten)
        .expect("terminal stub metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&kitten, permissions).expect("terminal stub permissions");
    let mut paths = vec![bin.to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(paths).expect("PATH with the terminal stub")
}

/// Liveness budget for waiting on a helper subprocess, shared with the crate's
/// own unit tests. See its documentation in `jjfx::test_support` for why it is
/// deliberately generous rather than a statement of expected duration.
use jjfx::test_support::LIVENESS_TIMEOUT;

/// The message Dispatch prints when it refuses to run because the terminal could
/// not be queried (ADR 0011).
const TERMINAL_REFUSAL: &str = "cannot determine which Worker Workspaces are open";

/// Runs a `jjfx` invocation to completion, retrying while Dispatch refused
/// because the terminal could not be queried.
///
/// Every Dispatch test drives the real terminal seam (ADR 0011): jjfx spawns
/// `kitten @ ls` and gives it a short deadline. The stub terminal answers
/// instantly, but under load that subprocess can be starved past the deadline,
/// so Dispatch fails closed - printing nothing to stdout and the terminal
/// refusal to stderr - before it reaches the behaviour the test is about. That
/// is an environmental transient, not a product failure, and a test that is not
/// about the terminal must not turn it into a red suite. So the fixture retries
/// until the seam answers, bounded by [`LIVENESS_TIMEOUT`]: a genuinely
/// unreachable or stuck terminal still fails the test within the bound rather
/// than hanging it.
///
/// The retry is deliberately narrow - it fires only on the terminal refusal, so
/// a real failure of the behaviour under test is returned at once, not retried.
///
/// `build` is called for each attempt, because a [`Command`] cannot be re-run.
pub fn run_jjfx_tolerating_a_starved_terminal(build: impl Fn() -> Command) -> Output {
    let deadline = Instant::now() + LIVENESS_TIMEOUT;
    loop {
        let output = build().output().expect("jjfx should run");
        if !refused_on_the_terminal_check(&output) || Instant::now() >= deadline {
            return output;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Like [`run_jjfx_tolerating_a_starved_terminal`], for an invocation that reads
/// its Ticket from stdin, which has to be piped in on every attempt.
pub fn run_jjfx_with_stdin_tolerating_a_starved_terminal(
    build: impl Fn() -> Command,
    stdin: &str,
) -> Output {
    let deadline = Instant::now() + LIVENESS_TIMEOUT;
    loop {
        let mut child = build()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("jjfx should run");
        child
            .stdin
            .as_mut()
            .expect("piped stdin")
            .write_all(stdin.as_bytes())
            .expect("write stdin");
        let output = child.wait_with_output().expect("jjfx output");
        if !refused_on_the_terminal_check(&output) || Instant::now() >= deadline {
            return output;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Whether `output` is Dispatch's fail-closed refusal to run the terminal check,
/// rather than a failure of the behaviour a test is asserting on.
fn refused_on_the_terminal_check(output: &Output) -> bool {
    !output.status.success() && String::from_utf8_lossy(&output.stderr).contains(TERMINAL_REFUSAL)
}

/// Polls `path` to a bounded deadline, returning its contents once non-empty.
///
/// Waiting for the path to *appear* is not enough for a file another process
/// writes: it exists before it is written. `fs::write` opens (and truncates) its
/// target before it writes, and a shell's `printf ... > file` does the same, so
/// a reader scheduled in that window parses an empty file. Under load the window
/// widens - the writer's next scheduling slice is further away - which is exactly
/// the process/timing flake this replaces: a `wait_for_file` followed by a bare
/// read could observe the empty window and panic on "runtime PID".
///
/// Use this wherever a test reads a file a helper writes; use `wait_for_file`
/// (existence only) for a barrier file that is deliberately created empty.
pub fn wait_for_file_content(path: &Path) -> String {
    let deadline = Instant::now() + LIVENESS_TIMEOUT;
    loop {
        if let Ok(contents) = fs::read_to_string(path)
            && !contents.trim().is_empty()
        {
            return contents;
        }
        assert!(
            Instant::now() < deadline,
            "{} should be written with content",
            path.display()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

/// How long a guard waits for a helper to record the Agent Runtime's process
/// group before reaping only the helper itself.
///
/// This runs on the failure path alone - a test that returns normally has
/// already disarmed its guard - so a short wait is invisible, and it covers the
/// window where a test panics before the runtime has written its PID.
const HELPER_REAP_TIMEOUT: Duration = Duration::from_secs(5);

/// Owns a helper subprocess and the Agent process groups it starts, so neither
/// outlives the test that spawned them.
///
/// A process/timing test runs the real supervisor in a separate process: it
/// spawns this test binary as a helper, and the helper spawns an Agent Runtime
/// into a process group of its own. Nothing reaps either on the failure path,
/// because a `Child` is not killed when it drops and a stubborn runtime ignores
/// `TERM`, so an assertion that panics part-way through a test leaves the
/// helper blocked on a runtime that never exits - a live `claude` process that
/// outlives the suite. The guard closes that hole: while it is armed it kills
/// the recorded process group(s) and then kills and reaps the helper, whether
/// the test returns or unwinds.
///
/// Waiting for the helper through [`HelperGuard::wait_with_output`] disarms the
/// guard, so a test that has observed the product clean up can still assert on
/// it: cleanup then belongs to the test, not the guard. [`HelperGuard::reap`]
/// reaps a helper that exits on its own while keeping the guard armed for the
/// process group it left behind.
#[must_use = "the guard reaps its helper only while it is alive"]
pub struct HelperGuard {
    child: Option<Child>,
    release: Option<PathBuf>,
    process_groups: Vec<PathBuf>,
    disarmed: bool,
}

impl HelperGuard {
    /// Takes ownership of `command`'s child.
    ///
    /// `release`, when given, is written on drop so a helper blocked on a
    /// barrier can exit on its own. `process_groups` name files holding the PID
    /// of an Agent Runtime the helper started; the group of the first PID found
    /// is killed on drop.
    pub fn spawn(command: &mut Command, release: Option<&Path>, process_groups: &[&Path]) -> Self {
        Self {
            child: Some(command.spawn().expect("helper process")),
            release: release.map(Path::to_owned),
            process_groups: process_groups
                .iter()
                .map(|path| (*path).to_owned())
                .collect(),
            disarmed: false,
        }
    }

    /// Reports whether the helper is still running.
    pub fn is_running(&mut self) -> bool {
        self.child
            .as_mut()
            .expect("helper process")
            .try_wait()
            .expect("helper status")
            .is_none()
    }

    /// Waits for the helper and disarms the guard.
    pub fn wait_with_output(mut self) -> Output {
        self.disarmed = true;
        self.child
            .take()
            .expect("helper process")
            .wait_with_output()
            .expect("helper output")
    }

    /// Reaps the helper without disarming the guard.
    ///
    /// For a helper that exits on its own but leaves a process group behind: a
    /// recorded leader that spawned a descendant and then exited. The group
    /// must still be killed if the test later unwinds, so unlike
    /// [`HelperGuard::wait_with_output`] this leaves cleanup with the guard. The
    /// child handle is dropped once reaped, so Drop never signals a PID the
    /// kernel has already released.
    pub fn reap(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
    }

    /// The process group an Agent Runtime the helper started leads, if it can be
    /// told within [`HELPER_REAP_TIMEOUT`].
    fn recorded_process_group(&self) -> Option<Pid> {
        if self.process_groups.is_empty() {
            return None;
        }
        let deadline = Instant::now() + HELPER_REAP_TIMEOUT;
        loop {
            if let Some(pid) = self
                .process_groups
                .iter()
                .find_map(|path| recorded_pid(path))
            {
                return Some(pid);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for HelperGuard {
    fn drop(&mut self) {
        // A consumed guard has already reaped its helper and handed cleanup back
        // to the test, so there is nothing left to own.
        if self.disarmed {
            return;
        }
        if let Some(release) = &self.release {
            let _ = fs::write(release, []);
        }
        if let Some(pid) = self.recorded_process_group() {
            let _ = kill_process_group(pid, Signal::KILL);
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Reads the PID a helper recorded in `path` and treats it as the process-group
/// it leads.
fn recorded_pid(path: &Path) -> Option<Pid> {
    let raw = fs::read_to_string(path)
        .ok()?
        .split_whitespace()
        .next()?
        .parse::<i32>()
        .ok()?;
    Pid::from_raw(raw)
}
