//! Test-only surface shared by the crate's own unit tests and the integration
//! suite.
//!
//! Each file under `tests/` is its own crate, so a value that both they and the
//! crate's `#[cfg(test)]` modules need has to live here: a private item is
//! invisible to the integration tests, and `tests/support` is invisible to the
//! unit tests. The module is compiled into every build, so it is
//! `#[doc(hidden)]` and the functions that drive the real terminal stay behind
//! the `test-support` feature rather than being compiled into a normal binary.

use std::fs::File;
use std::path::Path;
use std::time::Duration;

/// The one liveness budget every test waits on concurrent work with.
///
/// This is a guard against a *hang*, not a statement of how long the work is
/// expected to take. The waits it covers are on work whose wall-clock duration
/// tracks machine load rather than CPU: helper subprocesses doing real `jj`/`git`
/// work (creating repositories, `jj workspace add`, destroy, forge steps),
/// background engine work a unit test drives to completion, and threads waiting
/// on an advisory file lock. One helper has been measured burning only ~1.3s of
/// CPU while taking 7-48s of wall clock while several cargo builds shared the
/// machine, and under a four-way full-suite load a background step has exceeded
/// 30s. A budget tuned to the *expected* duration therefore goes red under load
/// with no regression behind it, which is the worst kind of failure: a re-run at
/// best, a wrong conclusion at worst.
///
/// So it is deliberately generous - roughly 2.5x the worst legitimate wait
/// observed above. Two minutes is invisible next to the cost of a false red, and
/// a genuinely hung helper still fails its test within this bound instead of
/// hanging the suite forever. Do not "tidy" it back down to a few seconds, and do
/// not add a second, tighter budget at a new wait: that reintroduces the
/// load-sensitive flake this exists to remove.
pub const LIVENESS_TIMEOUT: Duration = Duration::from_secs(120);

/// Hold jj's working-copy lock for as long as the returned file is alive,
/// exactly as a concurrent `jj` process does while it snapshots the working
/// copy.
///
/// jj takes an exclusive `flock(2)` on
/// `<root>/.jj/working_copy/working_copy.lock` for the whole of a working-copy
/// snapshot, and prints nothing while it waits for another process to release
/// it. A read that forgets `--ignore-working-copy` therefore blocks silently
/// behind the holder for as long as it runs. A test that proves a read is
/// lock-free holds this lock and requires the read to return anyway (dogfood
/// 08, 12).
pub fn hold_the_working_copy_lock(root: &Path) -> File {
    let lock = File::create(root.join(".jj/working_copy/working_copy.lock"))
        .expect("working-copy lock file");
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)
        .expect("hold the working-copy lock");
    lock
}

/// Enter the terminal and panic, for an external PTY restoration test.
#[cfg(feature = "test-support")]
pub fn panic_after_tui_init() -> ! {
    let _session = crate::tui::Session::enter().expect("test PTY should enter");
    panic!("intentional terminal restoration test panic");
}

/// Enter the terminal and hold it, for an external PTY signal test. The
/// process stays in raw mode on the alternate screen until the test signals
/// it, so the handler's restoration is the only thing that can clean up.
#[cfg(feature = "test-support")]
pub fn block_after_tui_init() -> ! {
    let _session = crate::tui::Session::enter().expect("test PTY should enter");
    loop {
        std::thread::sleep(Duration::from_millis(50));
    }
}
