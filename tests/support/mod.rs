//! Values shared by the integration tests.
//!
//! Each file directly under `tests/` is its own crate, so anything shared has to
//! live in a module like this one and be pulled in with `mod support;`.
//!
//! Because this module is compiled into every test crate, a helper used by only
//! some of them is dead code in the rest. That is the point of a shared toolbox,
//! so dead-code analysis is off here rather than worked around per crate.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process_group};

/// Liveness budget for waiting on a helper subprocess.
///
/// This is a guard against a *hang*, not a statement of how long the work is
/// expected to take. The helpers do real `jj`/`git` work - creating
/// repositories, `jj workspace add`, destroy, forge steps - whose wall-clock
/// duration tracks machine load rather than CPU: a single helper has been
/// measured burning only ~1.3s of CPU while taking 7-48s of wall clock while
/// several cargo builds shared the machine. A budget tuned to the *expected*
/// duration therefore goes red under load with no regression behind it, which
/// is the worst kind of failure: a re-run at best, a wrong conclusion at worst.
///
/// So it is deliberately generous - roughly 2.5x the worst legitimate wait
/// observed above. Two minutes is invisible next to the cost of a false red,
/// and a genuinely hung helper still fails its test within this bound instead of
/// hanging the suite forever. Do not "tidy" it back down to a few seconds; that
/// reintroduces the load-sensitive flake this exists to remove.
pub const HELPER_LIVENESS_TIMEOUT: Duration = Duration::from_secs(120);

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
    let deadline = Instant::now() + HELPER_LIVENESS_TIMEOUT;
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
/// it: cleanup then belongs to the test, not the guard.
#[must_use = "the guard reaps its helper only while it is alive"]
pub struct HelperGuard {
    child: Option<Child>,
    release: Option<PathBuf>,
    process_groups: Vec<PathBuf>,
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
        self.child
            .take()
            .expect("helper process")
            .wait_with_output()
            .expect("helper output")
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
        let Some(mut child) = self.child.take() else {
            return;
        };
        if let Some(release) = &self.release {
            let _ = fs::write(release, []);
        }
        if let Some(pid) = self.recorded_process_group() {
            let _ = kill_process_group(pid, Signal::KILL);
        }
        let _ = child.kill();
        let _ = child.wait();
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
