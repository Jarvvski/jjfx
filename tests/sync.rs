//! The behind-trunk warning and `jjfx sync` (ticket 13).
//!
//! After `jj git fetch` (or a PR merging elsewhere) advances `main`, `@` stays
//! where it was and the two become *siblings*: the files on disk are the old
//! tree, silently. These tests drive the real binary against a real jj repo to
//! prove the stale state is reported, that `jjfx sync` fixes both shapes (an
//! empty `@` and one carrying changes), and that a conflicted re-sync is
//! surfaced rather than resolved.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// A throwaway repository plus the config home the binary under test reads.
struct Fixture {
    root: TempDir,
    config_home: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().expect("temporary repository");
        let config_home = TempDir::new().expect("temporary config home");
        let fixture = Self { root, config_home };
        fixture.write_jj_config();
        fixture.jj_ok(&["git", "init", "--no-colocate"]);
        fixture
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    /// The jj config the binary under test reads.
    ///
    /// Passed as `JJ_CONFIG` rather than set with `jj config set --repo`: jj
    /// stores repo config under a directory derived from `XDG_CONFIG_HOME`,
    /// which this fixture points at its own config home, so a `--repo` write
    /// would land where the binary never looks (and a developer's own user
    /// config would hide that).
    fn jj_config_path(&self) -> PathBuf {
        self.config_home.path().join("jj.toml")
    }

    fn write_jj_config(&self) {
        fs::write(
            self.jj_config_path(),
            "user.name = \"jjfx test\"\n\
             user.email = \"jjfx@example.com\"\n\
             signing.behavior = \"drop\"\n",
        )
        .expect("jj config file");
    }

    fn jj(&self, args: &[&str]) -> Output {
        Command::new("jj")
            .args(args)
            .current_dir(self.path())
            .env("JJ_CONFIG", self.jj_config_path())
            .output()
            .expect("jj should be installed")
    }

    fn jj_ok(&self, args: &[&str]) -> String {
        let output = self.jj(args);
        assert!(
            output.status.success(),
            "jj {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn jjfx(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_jjfx"))
            .args(args)
            .current_dir(self.path())
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .env("JJ_CONFIG", self.jj_config_path())
            .output()
            .expect("jjfx should run")
    }

    /// A commit on `main`, then `main` advances along a sibling line, leaving the
    /// working copy on the old base - the exact shape a fetch produces.
    ///
    /// `carries_changes` leaves an edit in the working copy (deliberately not
    /// snapshotted), which is the shape that must be rebased rather than
    /// replaced.
    fn working_copy_beside_advanced_main(&self, carries_changes: bool) {
        fs::write(self.path().join("f.txt"), "base\n").expect("base file");
        self.jj_ok(&["commit", "-m", "base"]);
        let base = self.jj_ok(&["log", "-r", "@-", "--no-graph", "-T", "commit_id"]);
        self.jj_ok(&["bookmark", "create", "main", "-r", &base]);
        self.jj_ok(&["new", "-r", "main", "-m", "advance"]);
        fs::write(self.path().join("g.txt"), "advance\n").expect("advance file");
        self.jj_ok(&["commit", "-m", "advance"]);
        let advanced = self.jj_ok(&["log", "-r", "@-", "--no-graph", "-T", "commit_id"]);
        self.jj_ok(&["bookmark", "set", "main", "-r", &advanced]);
        self.jj_ok(&["new", &base]);
        if carries_changes {
            fs::write(self.path().join("f.txt"), "edited\n").expect("working-copy edit");
        }
    }

    /// Like [`Fixture::working_copy_beside_advanced_main`], but `main` also
    /// rewrites the file the working copy edits, so re-syncing must conflict.
    fn working_copy_beside_conflicting_main(&self) {
        fs::write(self.path().join("f.txt"), "base\n").expect("base file");
        self.jj_ok(&["commit", "-m", "base"]);
        let base = self.jj_ok(&["log", "-r", "@-", "--no-graph", "-T", "commit_id"]);
        self.jj_ok(&["bookmark", "create", "main", "-r", &base]);
        self.jj_ok(&["new", "-r", "main", "-m", "advance"]);
        fs::write(self.path().join("f.txt"), "theirs\n").expect("advance file");
        self.jj_ok(&["commit", "-m", "advance"]);
        let advanced = self.jj_ok(&["log", "-r", "@-", "--no-graph", "-T", "commit_id"]);
        self.jj_ok(&["bookmark", "set", "main", "-r", &advanced]);
        self.jj_ok(&["new", &base]);
        fs::write(self.path().join("f.txt"), "mine\n").expect("working-copy edit");
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A stale working copy is named by the state-reading commands, with how far
/// behind it is and the command that fixes it. `root` keeps its stdout a bare
/// path, so the warning can never corrupt a script reading it.
#[test]
fn a_working_copy_behind_trunk_is_reported_by_root_where_and_status() {
    let fixture = Fixture::new();
    fixture.working_copy_beside_advanced_main(false);

    let root = fixture.jjfx(&["root"]);
    assert!(root.status.success(), "{}", stderr(&root));
    assert_eq!(
        stdout(&root).trim(),
        fixture
            .path()
            .canonicalize()
            .expect("canonical root")
            .display()
            .to_string(),
        "root's stdout stays the bare path"
    );
    assert!(
        stderr(&root).contains("1 commit behind trunk"),
        "root should name how far behind: {}",
        stderr(&root)
    );
    assert!(
        stderr(&root).contains("jjfx sync"),
        "root should name the fix: {}",
        stderr(&root)
    );

    let where_output = fixture.jjfx(&["where"]);
    assert!(where_output.status.success(), "{}", stderr(&where_output));
    assert!(
        stderr(&where_output).contains("1 commit behind trunk"),
        "where should report the drift: {}",
        stderr(&where_output)
    );

    // `status` reads the Pool, so give it one first.
    let pool = fixture.jjfx(&["pool", "1"]);
    assert!(pool.status.success(), "{}", stderr(&pool));
    let status = fixture.jjfx(&["status"]);
    assert!(status.status.success(), "{}", stderr(&status));
    assert!(
        stderr(&status).contains("1 commit behind trunk"),
        "status should report the drift: {}",
        stderr(&status)
    );
    assert!(
        stderr(&status).contains("jjfx sync"),
        "status should name the fix: {}",
        stderr(&status)
    );
}

/// An empty `@` is replaced with a fresh commit directly above trunk, and the
/// warning is gone afterwards.
#[test]
fn sync_moves_an_empty_working_copy_onto_trunk() {
    let fixture = Fixture::new();
    fixture.working_copy_beside_advanced_main(false);

    let output = fixture.jjfx(&["sync"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("Moved the empty working copy onto trunk"),
        "{}",
        stderr(&output)
    );
    let where_output = fixture.jjfx(&["where"]);
    assert!(
        !stderr(&where_output).contains("behind trunk"),
        "the drift should be gone: {}",
        stderr(&where_output)
    );
    assert_eq!(
        fixture.jj_ok(&["log", "-r", "@-", "--no-graph", "-T", "commit_id"]),
        fixture.jj_ok(&["log", "-r", "main", "--no-graph", "-T", "commit_id"]),
        "the fresh working copy sits directly above main"
    );
}

/// A working copy that carries changes is rebased, not replaced, so the edit
/// survives the move.
#[test]
fn sync_rebases_a_working_copy_that_carries_changes() {
    let fixture = Fixture::new();
    fixture.working_copy_beside_advanced_main(true);

    let output = fixture.jjfx(&["sync"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("Rebased the working copy onto trunk"),
        "{}",
        stderr(&output)
    );
    assert_eq!(
        fs::read_to_string(fixture.path().join("f.txt")).expect("edited file"),
        "edited\n",
        "the edit that was only on disk before the sync must survive it"
    );
    assert_eq!(
        fs::read_to_string(fixture.path().join("g.txt")).expect("advance file"),
        "advance\n",
        "the moved working copy also carries main's advance"
    );
}

/// A re-sync that hits a conflict exits non-zero, names the conflicted commit,
/// and leaves the conflict on disk for a human to resolve.
#[test]
fn sync_surfaces_a_conflict_instead_of_resolving_it() {
    let fixture = Fixture::new();
    fixture.working_copy_beside_conflicting_main();

    let output = fixture.jjfx(&["sync"]);

    assert!(
        !output.status.success(),
        "a conflicted re-sync must not report success: {}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("conflicted commit"),
        "{}",
        stderr(&output)
    );
    assert!(stderr(&output).contains("resolve"), "{}", stderr(&output));
    let content = fs::read_to_string(fixture.path().join("f.txt")).expect("conflicted file");
    assert!(content.contains("<<<<<<<"), "{content}");
}

/// A working copy already above trunk is left alone.
#[test]
fn sync_on_a_current_working_copy_is_a_no_op() {
    let fixture = Fixture::new();
    fs::write(fixture.path().join("f.txt"), "base\n").expect("base file");
    fixture.jj_ok(&["commit", "-m", "base"]);
    fixture.jj_ok(&["bookmark", "create", "main", "-r", "@-"]);
    fixture.jj_ok(&["new", "main"]);

    let output = fixture.jjfx(&["sync"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("Already on trunk"),
        "{}",
        stderr(&output)
    );
}

/// A fetch that fails is a warning, not a failure: the move onto the local trunk
/// is the remedy and must not be blocked by the network.
#[test]
fn sync_still_moves_onto_trunk_when_the_fetch_fails() {
    let fixture = Fixture::new();
    fixture.working_copy_beside_advanced_main(false);
    let missing = fixture.path().join("missing-origin.git");
    fixture.jj_ok(&["git", "remote", "add", "origin", &missing.to_string_lossy()]);

    let output = fixture.jjfx(&["sync"]);

    assert!(
        output.status.success(),
        "a failed fetch must not block the move: {}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("could not fetch"),
        "the failed fetch should be surfaced: {}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("Moved the empty working copy onto trunk"),
        "{}",
        stderr(&output)
    );
}
