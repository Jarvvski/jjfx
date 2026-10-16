//! Dispatch must not report success when nothing happened.
//!
//! Each case drives the real binary, because the point is the exit code and the
//! output a scripting caller sees: a batch where every Ticket failed must exit
//! non-zero, a batch that dispatched (or held back) a Ticket must exit zero, and
//! a background orchestration that fails must be reported as a failure rather
//! than as `orchestrating`.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use jjfx::engine::{
    DispatchGroupOptions, DispatchGroupState, Expected, Repository, StateChange, SubIssueState,
    TicketId, WireStatus, WireTimestamp,
};
use tempfile::TempDir;

/// A jj repository with one local ready Ticket and a `[tickets] source = "local"`
/// config home. The Pool and remote are added per case, because they are what
/// decide which phase a Dispatch fails in.
struct DispatchFixture {
    root: TempDir,
    config_home: TempDir,
    bin: TempDir,
}

impl DispatchFixture {
    fn new() -> Self {
        let root = TempDir::new().expect("temporary repository");
        let config_home = TempDir::new().expect("temporary config home");
        let bin = TempDir::new().expect("temporary bin");
        let fixture = Self {
            root,
            config_home,
            bin,
        };
        fixture.jj(&["git", "init", "--no-colocate"]);
        fixture.write_ticket(
            "widgets",
            "01-ship-the-widget.md",
            "# Ship the widget\n\nStatus: ready-for-agent\n\n## Problem\n\nDo it.\n",
        );
        fixture.write_config("[tickets]\nsource = \"local\"\n");
        fixture
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    /// The jj config file the binary under test reads its delivery identity from.
    ///
    /// It is passed as `JJ_CONFIG` rather than set with `jj config set --repo`:
    /// jj stores repo config under a directory derived from `XDG_CONFIG_HOME`,
    /// which this fixture points at its own config home, so a `--repo` write
    /// would land where the binary never looks (and a developer's own user
    /// config would hide that).
    fn jj_config_path(&self) -> std::path::PathBuf {
        self.config_home.path().join("jj.toml")
    }

    fn jj(&self, args: &[&str]) -> Output {
        Command::new("jj")
            .args(["--config", "signing.behavior=drop"])
            .args(args)
            .current_dir(self.path())
            .output()
            .expect("jj should be installed")
    }

    fn write_ticket(&self, feature: &str, file: &str, content: &str) {
        let directory = self.path().join(".scratch").join(feature).join("issues");
        fs::create_dir_all(&directory).expect("issues directory");
        fs::write(directory.join(file), content).expect("Ticket file");
    }

    fn write_config(&self, content: &str) {
        let directory = self.config_home.path().join("jjfx");
        fs::create_dir_all(&directory).expect("config directory");
        fs::write(directory.join("config.toml"), content).expect("config file");
        fs::write(
            self.jj_config_path(),
            "user.name = \"Test User\"\nuser.email = \"test@example.com\"\n",
        )
        .expect("jj config file");
    }

    /// Sizes the Pool, so Dispatch reaches a real per-Ticket outcome rather than
    /// failing on a missing Pool.
    fn with_pool(&self, size: usize) {
        let output = self
            .jjfx(&["pool", &size.to_string()])
            .output()
            .expect("pool");
        assert!(output.status.success(), "{}", stderr(&output));
    }

    /// Gives the repository an `origin` remote, so Dispatch can resolve the
    /// owner/name identity a Pull Request targets.
    fn with_origin(&self) {
        assert!(
            self.jj(&[
                "git",
                "remote",
                "add",
                "origin",
                "git@github.com:owner/name.git"
            ])
            .status
            .success()
        );
    }

    /// Puts a fake `claude` on PATH that exits successfully, so a Dispatch can
    /// launch a Run without a real Agent Runtime. Returns the PATH to use.
    fn with_fake_agent(&self) -> std::ffi::OsString {
        let agent = self.bin.path().join("claude");
        fs::write(
            &agent,
            "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'\n",
        )
        .expect("fake agent");
        fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).expect("executable agent");
        let mut paths = vec![self.bin.path().to_path_buf()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        std::env::join_paths(paths).expect("runtime PATH")
    }

    fn jjfx(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_jjfx"));
        command
            .args(args)
            .current_dir(self.path())
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .env("JJ_CONFIG", self.jj_config_path());
        command
    }

    /// Persists a terminal Dispatch Group for `parent`, so orchestration resumes
    /// it rather than discovering children - which a local Ticket source cannot
    /// do for a Parent that is also a Ticket.
    fn persist_terminal_group(&self, parent: &str) {
        let repository = Repository::open(self.path()).expect("open repository");
        let parent = TicketId::parse(parent).expect("Parent Ticket");
        let mut group = DispatchGroupState::new(
            parent.clone(),
            WireTimestamp::new("2026-08-04T12:00:00Z"),
            "owner/repo",
            DispatchGroupOptions::new(""),
        );
        group.sub_issues.insert(
            TicketId::parse("widgets--02-dependent").expect("Sub-issue"),
            SubIssueState::new("Failed", WireStatus::new("failed"), Vec::new()),
        );
        repository
            .state_store()
            .dispatch_group(parent)
            .commit(Expected::Missing, StateChange::Replace(group))
            .expect("save group");
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_batch_where_every_ticket_failed_exits_non_zero() {
    let fixture = DispatchFixture::new();
    fixture.with_pool(1);
    // No origin remote, so the only Ticket fails at the identity phase - the
    // exact shape that used to exit 0.
    let output = fixture
        .jjfx(&["dispatch", "--all", "--json", "--force"])
        .output()
        .expect("jjfx should run");

    assert!(
        !output.status.success(),
        "an all-failed batch must exit non-zero:\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(
        stdout(&output).contains("\"outcome\": \"failed\""),
        "{}",
        stdout(&output)
    );
    assert!(
        stderr(&output).contains("every Ticket failed to dispatch"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_batch_with_a_dispatched_ticket_exits_zero() {
    let fixture = DispatchFixture::new();
    fixture.with_pool(1);
    fixture.with_origin();
    let path = fixture.with_fake_agent();
    let output = fixture
        .jjfx(&["dispatch", "--all", "--json", "--force"])
        .env("PATH", path)
        .output()
        .expect("jjfx should run");

    assert!(
        output.status.success(),
        "a dispatched Ticket is a success:\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert!(
        stdout(&output).contains("\"outcome\": \"dispatched\""),
        "{}",
        stdout(&output)
    );
}

#[test]
fn a_background_orchestration_without_a_pool_is_a_failure_not_orchestrating() {
    let fixture = DispatchFixture::new();
    // No Pool at all: the child used to die at once behind a printed
    // `orchestrating`. The parent must surface the error and exit non-zero.
    let output = fixture
        .jjfx(&["dispatch", "widgets--01-ship-the-widget", "--json"])
        .output()
        .expect("jjfx should run");

    assert!(!output.status.success(), "{}", stdout(&output));
    assert!(
        !stdout(&output).contains("orchestrating"),
        "a dead orchestration must not print orchestrating:\n{}",
        stdout(&output)
    );
    assert!(
        stderr(&output).contains("Worker Pool is missing"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_background_orchestration_for_an_unresolvable_ticket_is_a_failure() {
    let fixture = DispatchFixture::new();
    fixture.with_pool(1);
    let output = fixture
        .jjfx(&["dispatch", "nope-typo", "--json"])
        .output()
        .expect("jjfx should run");

    assert!(!output.status.success(), "{}", stdout(&output));
    assert!(
        !stdout(&output).contains("orchestrating"),
        "a Ticket that cannot be resolved must not print orchestrating:\n{}",
        stdout(&output)
    );
    assert!(
        stderr(&output).contains("cannot fetch Ticket nope-typo"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_prepared_background_orchestration_names_the_child_log() {
    let fixture = DispatchFixture::new();
    fixture.with_pool(1);
    // A persisted group makes `prepare` resume rather than discover, so the
    // handoff reaches a real background child without a tracker.
    fixture.persist_terminal_group("widgets--01-ship-the-widget");

    let output = fixture
        .jjfx(&["dispatch", "widgets--01-ship-the-widget", "--json"])
        .output()
        .expect("jjfx should run");

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let json = stdout(&output);
    assert!(json.contains("\"outcome\": \"orchestrating\""), "{json}");
    let log = fixture
        .path()
        .join(".jj/pool/orchestrate-widgets--01-ship-the-widget.log");
    assert!(
        json.contains(&log.display().to_string()),
        "the JSON must name the child log:\n{json}"
    );
    assert!(
        log.is_file(),
        "the child log must exist at {}",
        log.display()
    );
}
