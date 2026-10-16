//! Values shared by the integration tests.
//!
//! Each file directly under `tests/` is its own crate, so anything shared has to
//! live in a module like this one and be pulled in with `mod support;`.
//!
//! Because this module is compiled into every test crate, a helper used by only
//! some of them is dead code in the rest. That is the point of a shared toolbox,
//! so dead-code analysis is off here rather than worked around per crate.
#![allow(dead_code)]

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

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
        if let Ok(contents) = std::fs::read_to_string(path)
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
