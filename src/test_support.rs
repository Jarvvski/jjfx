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

/// The jj config a throwaway test repository owns, so a fixture never inherits
/// the developer's `~/.config/jj/config.toml`.
///
/// Two of the developer's settings make a test pass locally and fail on a
/// clean-config CI runner: a `[user]` identity (without one a commit or push
/// fails with "Won't push commit ... since it has no author and/or committer
/// set") and `remotes.origin.auto-track-bookmarks` (which makes a fresh
/// bookmark list twice, once with a tracked-but-absent remote entry - dogfood-2
/// 02). The throwaway-repository helpers write this into the repository's
/// *own* config, because a unit test cannot set `JJ_CONFIG` on the `jj`
/// processes jjfx spawns; the repository then carries its identity and its
/// auto-tracking choice whatever the developer has configured.
///
/// `auto_track_origin` flips auto-tracking on, for the tests that assert the
/// tracked-but-absent state rather than the default.
pub fn hermetic_jj_config(auto_track_origin: bool) -> String {
    let auto_track = if auto_track_origin {
        "glob:*"
    } else {
        // jj parses this value as a string pattern and rejects an empty string
        // as a syntax error, so "auto-track nothing" is a glob that matches no
        // bookmark name.
        "glob:jjfx-hermetic-never-matches-a-bookmark"
    };
    format!(
        "[user]\nname = \"jjfx test\"\nemail = \"jjfx@example.com\"\n\n[remotes.origin]\nauto-track-bookmarks = \"{auto_track}\"\n"
    )
}

/// Make the Workspace at `workspace_dir`'s working-copy commit **divergent**: two
/// visible commits sharing one change id, the state a concurrent `jj` leaves
/// behind (bug-hunt tickets 44 and 46).
///
/// Deterministic rather than raced: two operations are derived from the *same*
/// starting operation (jj's `--at-operation`) and each rewrites the Workspace's
/// working-copy commit - one change id, two commit ids - then both are integrated
/// (`jj op integrate`). jj reconciles them into a divergent change, exactly as the
/// concurrent `undo --allow-cross-workspace` race did, without the flakiness of a
/// real race. The caller owns the repository and its jj config; this only drives
/// the operations.
pub fn diverge_working_copy(workspace_dir: &Path) {
    let run = |args: &[&str]| -> String {
        let output = std::process::Command::new("jj")
            .args(["--config", "signing.behavior=drop"])
            .args(args)
            .current_dir(workspace_dir)
            .output()
            .expect("jj should be installed");
        assert!(
            output.status.success(),
            "jj {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    };
    let start = run(&[
        "--ignore-working-copy",
        "op",
        "log",
        "--no-graph",
        "-n",
        "1",
        "-T",
        "id",
    ]);
    let start = start.trim().to_owned();
    let rewrite = |description: &str| -> String {
        let out = run(&[
            "--at-operation",
            &start,
            "describe",
            "-m",
            description,
            "--no-integrate-operation",
        ]);
        out.split_whitespace()
            .last()
            .expect("jj prints the uncommitted operation id")
            .to_owned()
    };
    let first = rewrite("a");
    let second = rewrite("b");
    run(&["op", "integrate", &first]);
    run(&["op", "integrate", &second]);
}

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
