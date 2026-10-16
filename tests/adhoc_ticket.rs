//! Ad-hoc Ticket input: free text, a file, and stdin, in a repository that has
//! no `[tickets]` configuration and no `.scratch/` tree at all.
//!
//! Each case drives the real binary, because the point is the whole path: a
//! Ticket handed in directly must reach Dispatch without ever consulting a
//! tracker - the default source is Linear, which wants an API key the
//! repository does not have.

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

mod support;

/// A jj repository with no jjfx `[tickets]` section and no `.scratch/`.
struct BareRepository {
    root: TempDir,
    config_home: TempDir,
    /// Holds the stub `kitten` binary the fixture puts on `PATH`; only its
    /// lifetime matters, so it is never read.
    _bin: TempDir,
    /// `PATH` with the stub terminal ahead of the inherited one, so Dispatch's
    /// terminal check (ADR 0011) answers deterministically and needs no
    /// `--force`.
    env_path: OsString,
}

impl BareRepository {
    fn new() -> Self {
        let root = TempDir::new().expect("temporary repository");
        let status = Command::new("jj")
            .args([
                "--config",
                "signing.behavior=drop",
                "git",
                "init",
                "--no-colocate",
            ])
            .arg(root.path())
            .status()
            .expect("jj should be installed");
        assert!(status.success(), "jj git init");
        let config_home = TempDir::new().expect("temporary config home");
        let bin = TempDir::new().expect("temporary stub bin");
        let env_path = support::stub_terminal(bin.path());
        let repository = Self {
            root,
            config_home,
            _bin: bin,
            env_path,
        };
        // The whole premise: nothing configures a Ticket source, and no local
        // Ticket tree exists. Only the bare jj repo does.
        assert!(!repository.config_file().exists());
        assert!(!repository.path().join(".scratch").exists());
        repository
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn config_file(&self) -> std::path::PathBuf {
        self.config_home.path().join("jjfx").join("config.toml")
    }

    fn jjfx(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_jjfx"));
        command
            .args(args)
            .current_dir(self.root.path())
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .env("PATH", &self.env_path);
        command
    }

    /// Creates an empty Pool, so Dispatch reaches the capacity governor rather
    /// than failing on a missing Pool.
    fn empty_pool(&self) {
        let output = self.jjfx(&["pool", "0"]).output().expect("jjfx pool 0");
        assert!(output.status.success(), "{}", stderr(&output));
    }

    /// Runs `dispatch` with `stdin` piped in, and returns the raw output.
    fn dispatch(&self, args: &[&str], stdin: &str) -> Output {
        let mut child = self
            .jjfx(args)
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
        child.wait_with_output().expect("jjfx output")
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn free_text_dispatches_with_no_ticket_source_configured() {
    let repository = BareRepository::new();
    repository.empty_pool();

    // `n` declines the capacity prompt, so the empty Pool holds the Ticket back
    // instead of growing - the id in the result proves the text reached Dispatch.
    let output = repository.dispatch(
        &[
            "dispatch",
            "--text",
            "Fix the login bug\n\nIt fails on Safari.",
            "--json",
        ],
        "n\n",
    );

    let stderr = stderr(&output);
    assert!(!stderr.contains("Linear"), "{stderr}");
    let json = stdout(&output);
    assert!(json.contains("ad-hoc--fix-the-login-bug"), "{json}");
    assert!(json.contains("\"phase\": \"capacity\""), "{json}");
}

#[test]
fn a_markdown_file_outside_scratch_dispatches() {
    let repository = BareRepository::new();
    repository.empty_pool();
    // A file wherever it lives, not the `.scratch/<feature>/issues/` convention.
    let directory = repository.path().join("docs");
    fs::create_dir_all(&directory).expect("docs directory");
    fs::write(
        directory.join("plan.md"),
        "# Ship the plan\n\n## Problem\n\nDo the work.\n",
    )
    .expect("Ticket file");

    let output = repository.dispatch(
        &["dispatch", "--from-file", "docs/plan.md", "--json"],
        "n\n",
    );

    let json = stdout(&output);
    assert!(json.contains("ad-hoc--plan"), "{json}");
    assert!(json.contains("\"phase\": \"capacity\""), "{json}");
}

#[test]
fn a_ticket_can_be_read_from_stdin() {
    let repository = BareRepository::new();

    // Empty stdin is read as the Ticket, and an empty Ticket is a loud error
    // rather than a silent no-op: this is what proves stdin is the input.
    let empty = repository.dispatch(&["dispatch", "-"], "");
    assert!(!empty.status.success(), "{}", stdout(&empty));
    assert!(
        stderr(&empty).contains("the Ticket text is empty"),
        "{}",
        stderr(&empty)
    );

    // Real text is carried through to Dispatch. With no Pool at all, the
    // failure is the Worker Pool - never a tracker.
    let piped = repository.dispatch(&["dispatch", "--from-file", "-"], "Fix it\n");
    let stderr = stderr(&piped);
    assert!(stderr.contains("Worker Pool"), "{stderr}");
    assert!(!stderr.contains("Linear"), "{stderr}");
}

#[test]
fn an_explicit_id_still_needs_no_tracker() {
    // `--id` names the ad-hoc Ticket; the failure is still the missing Pool,
    // never an unavailable Linear key.
    let repository = BareRepository::new();

    let output = repository.dispatch(&["dispatch", "--text", "Do it", "--id", "TOOL-9"], "n\n");

    let stderr = stderr(&output);
    assert!(stderr.contains("Worker Pool"), "{stderr}");
    assert!(!stderr.contains("Linear"), "{stderr}");
}
