//! A declared Worker Pool's state materialises on first use (ticket 13).
//!
//! The `[pools.<name>]` declaration is the source of truth and `.jj/pools/<name>/`
//! is state (ADR 0013), so a declared pool with no directory is normal - a lane a
//! teammate committed. The last mile is that the lane becomes usable: the first
//! Dispatch into it provisions the Workers the declaration names, exactly as
//! `jjfx pool <N>` does for `default`. These tests drive the real binary against a
//! real jj repository and provision nothing by hand: the product must create the
//! lane's state, its Workers and their Workspaces on its own.
#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

mod support;

/// A throwaway repository with an origin, a fake Agent Runtime and a stub
/// terminal, so a Dispatch can launch a Run without a real agent or a real kitty.
/// No test here writes pool or Worker state: that is the point.
///
/// Worker Workspaces are pinned under `<root>/ws` with `JJ_WS_DIR` so the fixture
/// owns every path it creates (a sibling `<repo>-workspaces` directory would sit
/// beside the `TempDir` and outlive it).
struct MaterialiseFixture {
    root: TempDir,
    config_home: TempDir,
    bin: TempDir,
    /// `PATH` with the fixture's `bin` ahead of the inherited one, so the stub
    /// terminal and the fake Agent are what the binary under test finds.
    env_path: OsString,
}

impl MaterialiseFixture {
    fn new() -> Self {
        let root = TempDir::new().expect("temporary repository");
        let config_home = TempDir::new().expect("temporary config home");
        let bin = TempDir::new().expect("temporary bin");
        let env_path = support::stub_terminal(bin.path());
        let fixture = Self {
            root,
            config_home,
            bin,
            env_path,
        };
        fs::write(
            fixture.jj_config_path(),
            "user.name = \"jjfx test\"\nuser.email = \"jjfx@example.com\"\n",
        )
        .expect("jj config");
        fixture.write_config("[tickets]\nsource = \"local\"\n");
        fixture.jj_ok(&["git", "init", "--no-colocate"]);
        fixture.jj_ok(&[
            "git",
            "remote",
            "add",
            "origin",
            "git@github.com:owner/name.git",
        ]);
        // A base commit carrying `main`, so trunk resolves and a Worker Workspace
        // can be added against it.
        fs::write(fixture.path().join("base.txt"), "base\n").expect("base file");
        fixture.jj_ok(&["commit", "-m", "base"]);
        fixture.jj_ok(&["bookmark", "create", "main", "-r", "@-"]);
        fixture.with_fake_agent();
        fixture
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn ws_dir(&self) -> PathBuf {
        self.path().join("ws")
    }

    fn pool_state(&self, pool: &str) -> PathBuf {
        self.path().join(".jj/pools").join(pool)
    }

    fn jj_config_path(&self) -> PathBuf {
        self.config_home.path().join("jj.toml")
    }

    fn jj_ok(&self, args: &[&str]) -> String {
        let output = Command::new("jj")
            .args(["--config", "signing.behavior=drop"])
            .args(args)
            .current_dir(self.path())
            .env("JJ_CONFIG", self.jj_config_path())
            .output()
            .expect("jj should be installed");
        assert!(
            output.status.success(),
            "jj {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn write_config(&self, content: &str) {
        let directory = self.config_home.path().join("jjfx");
        fs::create_dir_all(&directory).expect("config directory");
        fs::write(directory.join("config.toml"), content).expect("config file");
    }

    /// Declare the lanes, as `[pools.<name>]` sections. Nothing else is written.
    fn write_repo_config(&self, content: &str) {
        let directory = self.path().join(".jjfx");
        fs::create_dir_all(&directory).expect("repository config directory");
        fs::write(directory.join("config.toml"), content).expect("repository config file");
    }

    /// Writes one Ticket into the `widgets` feature.
    fn write_ticket(&self, file: &str, content: &str) {
        let directory = self.path().join(".scratch").join("widgets").join("issues");
        fs::create_dir_all(&directory).expect("issues directory");
        fs::write(directory.join(file), content).expect("Ticket file");
    }

    /// Puts a fake `claude` on the fixture's `PATH` that exits successfully, so a
    /// Dispatch can launch a Run without a real Agent Runtime.
    fn with_fake_agent(&self) {
        let agent = self.bin.path().join("claude");
        fs::write(
            &agent,
            "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'\n",
        )
        .expect("fake agent");
        fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).expect("executable agent");
    }

    fn jjfx(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_jjfx"))
            .args(args)
            .current_dir(self.path())
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .env("JJ_CONFIG", self.jj_config_path())
            .env("JJ_WS_DIR", self.ws_dir())
            .env("PATH", &self.env_path)
            .output()
            .expect("jjfx should run")
    }

    /// The Workers the lane's own state lists, in Pool order.
    fn workers(&self, pool: &str) -> Vec<String> {
        let text = fs::read_to_string(self.pool_state(pool).join("pool.json"))
            .expect("the lane's materialised state");
        let value: Value = serde_json::from_str(&text).expect("pool state is JSON");
        value["workers"]
            .as_array()
            .expect("a workers array")
            .iter()
            .map(|worker| worker.as_str().expect("a Worker id").to_owned())
            .collect()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn bug_ticket(title: &str) -> String {
    format!("# {title}\n\nStatus: ready-for-agent\nLabels: bug\n\n## Problem\n\nDo it.\n")
}

/// One Ticket object from a `dispatch --json` batch, by Ticket id.
fn entry<'a>(json: &'a Value, ticket: &str) -> &'a Value {
    json.as_array()
        .expect("a batch is an array")
        .iter()
        .find(|entry| entry["ticket"] == ticket)
        .unwrap_or_else(|| panic!("no entry for {ticket}: {json}"))
}

/// A declared lane with no state directory at all - the shape a fresh clone of a
/// Repository that committed a `[pools.bugs]` declaration has.
fn declared_lane() -> MaterialiseFixture {
    let fixture = MaterialiseFixture::new();
    fixture.write_repo_config("[pools.bugs]\nsize = 1\nlabel = \"bug\"\n");
    assert!(
        !fixture.pool_state("bugs").exists(),
        "the fixture must start with no `bugs` state"
    );
    fixture
}

/// The last mile: a declared lane becomes usable on its first Dispatch, with no
/// hand-provisioning of its state, its Workers or their Workspaces.
#[test]
fn dispatch_into_a_freshly_declared_pool_provisions_it_without_help() {
    let fixture = declared_lane();
    fixture.write_ticket("01-bug.md", &bug_ticket("Fix the crash"));

    let output = fixture.jjfx(&["dispatch", "--all", "--pool", "bugs", "--json"]);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let json: Value = serde_json::from_str(stdout(&output).trim()).expect("a JSON batch");
    assert_eq!(entry(&json, "widgets--01-bug")["outcome"], "dispatched");

    // The product created the lane's state and its Worker, and launched the Run
    // there - nothing in this test wrote pool or Worker state.
    let workers = fixture.workers("bugs");
    assert_eq!(
        workers.len(),
        1,
        "the declared size is honoured: {workers:?}"
    );
    let worker = &workers[0];
    let state = fs::read_to_string(fixture.pool_state("bugs").join(format!("{worker}.json")))
        .expect("the materialised Worker state");
    assert!(state.contains("\"ticket\": \"widgets--01-bug\""), "{state}");
    // And the Worker's jj Workspace exists, so the Run had somewhere to execute.
    assert!(
        fixture.ws_dir().join(worker).is_dir(),
        "the Worker Workspace must be provisioned"
    );
}

/// Materialisation is idempotent: a second Dispatch into the lane never
/// re-provisions it, so its Workers are not duplicated.
#[test]
fn materialising_a_declared_pool_is_idempotent() {
    let fixture = declared_lane();
    fixture.write_ticket("01-bug.md", &bug_ticket("Fix the crash"));
    let first = fixture.jjfx(&["dispatch", "--all", "--pool", "bugs", "--json"]);
    assert!(first.status.success(), "{}", stderr(&first));
    let after_first = fixture.workers("bugs");
    assert_eq!(after_first.len(), 1);

    fixture.write_ticket("02-bug.md", &bug_ticket("Fix the other crash"));
    // The second Dispatch may dispatch to the idle Worker or be held back while it
    // is busy; either way the lane's state must be untouched.
    fixture.jjfx(&["dispatch", "--all", "--pool", "bugs", "--json"]);

    assert_eq!(
        fixture.workers("bugs"),
        after_first,
        "a second Dispatch must not re-provision the lane"
    );
}

/// A Pool that is declared and then removed leaves nothing behind: materialising
/// a lane must never resurrect a directory removal has taken.
#[test]
fn a_removed_pool_is_not_resurrected_by_materialisation() {
    let fixture = declared_lane();
    fixture.write_ticket("01-bug.md", &bug_ticket("Fix the crash"));
    let dispatched = fixture.jjfx(&["dispatch", "--all", "--pool", "bugs", "--json"]);
    assert!(dispatched.status.success(), "{}", stderr(&dispatched));
    assert!(fixture.pool_state("bugs").join("pool.json").exists());

    let removed = fixture.jjfx(&["pool", "destroy", "bugs", "--force"]);
    assert!(removed.status.success(), "{}", stderr(&removed));
    assert!(!fixture.pool_state("bugs").exists());

    // A read and a stale Dispatch both leave the removed lane gone rather than
    // materialising it back into existence. (`status` may legitimately report the
    // pre-pools "No pool" guidance now that nothing is declared; the directory is
    // what must stay absent.)
    fixture.jjfx(&["status"]);
    assert!(
        !fixture.pool_state("bugs").exists(),
        "a read must not materialise a removed pool"
    );
    let stale = fixture.jjfx(&["dispatch", "--all", "--pool", "bugs", "--json"]);
    assert!(
        !stale.status.success(),
        "a removed pool is not held, so a Dispatch naming it must refuse"
    );
    assert!(
        stderr(&stale).contains("bugs"),
        "the refusal names the pool: {}",
        stderr(&stale)
    );
    assert!(
        !fixture.pool_state("bugs").exists(),
        "materialisation must not resurrect a removed pool"
    );
}
