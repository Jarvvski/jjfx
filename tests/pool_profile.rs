//! `jjfx pool profile` works before the Pool exists (dogfood ticket 04).
//!
//! The documented dispatch order names the Agent Runtime before pool capacity,
//! but the profile is stored in the Pool document - so on a Repository with no
//! Pool state yet the command used to fail with a bare `state is missing`, which
//! names neither the missing state nor the command that creates it. Naming a
//! runtime now provisions the empty Pool document it needs, exactly as
//! `jjfx pool <N>` does, and provisions no capacity: `jjfx pool <N>` remains the
//! only thing that creates Workers.
//!
//! These tests drive the real binary against a real jj repository and write no
//! Pool state themselves - the product must create it.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

/// A real jj Repository with an origin and a config home of its own, so the
/// binary under test never reads the developer's own config. Worker Workspaces
/// are pinned under `<root>/ws` with `JJ_WS_DIR` so the fixture owns every path
/// it creates.
struct Fixture {
    root: TempDir,
    config_home: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().expect("temporary repository");
        let config_home = TempDir::new().expect("temporary config home");
        let fixture = Self { root, config_home };
        fs::write(
            fixture.jj_config_path(),
            "user.name = \"jjfx test\"\nuser.email = \"jjfx@example.com\"\n",
        )
        .expect("jj config");
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
        fixture
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn ws_dir(&self) -> PathBuf {
        self.path().join("ws")
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

    fn jjfx(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_jjfx"))
            .args(args)
            .current_dir(self.path())
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .env("JJ_CONFIG", self.jj_config_path())
            .env("JJ_WS_DIR", self.ws_dir())
            .output()
            .expect("jjfx should run")
    }

    /// The `default` Pool document, or `None` when no Pool state exists.
    fn pool_state(&self) -> Option<Value> {
        let path = self.path().join(".jj/pools/default/pool.json");
        let text = fs::read_to_string(path).ok()?;
        Some(serde_json::from_str(&text).expect("Pool state is JSON"))
    }

    /// The `default` Pool document, asserting it exists.
    fn pool_json(&self) -> Value {
        self.pool_state().expect("the Pool document")
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Naming a runtime on a Repository with no Pool state succeeds, records the
/// runtime, and creates no capacity - the error must never be the bare
/// `state is missing`.
#[test]
fn profile_before_pool_state_is_recorded_without_capacity() {
    let fixture = Fixture::new();
    assert!(
        fixture.pool_state().is_none(),
        "the fixture must start with no Pool state"
    );

    let output = fixture.jjfx(&["pool", "profile", "opencode"]);

    assert!(output.status.success(), "stderr:\n{}", stderr(&output));
    assert!(
        !stderr(&output).contains("state is missing"),
        "the refusal must name what is missing: {}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("opencode"),
        "the confirmation names the runtime: {}",
        stderr(&output)
    );

    let pool = fixture.pool_json();
    assert_eq!(pool["agent"], "opencode");
    assert_eq!(
        pool["size"], 0,
        "naming a runtime must not provision capacity"
    );
    assert_eq!(
        pool["workers"].as_array().map(Vec::len),
        Some(0),
        "naming a runtime must not create Workers"
    );
    assert!(
        !fixture.ws_dir().exists(),
        "naming a runtime must not create a Worker Workspace"
    );
}

/// A runtime named before the Pool exists survives the Pool being sized, so the
/// documented profile-then-capacity order is a working one.
#[test]
fn a_profile_set_before_the_pool_survives_sizing_it() {
    let fixture = Fixture::new();
    let profile = fixture.jjfx(&["pool", "profile", "opencode"]);
    assert!(profile.status.success(), "{}", stderr(&profile));

    let sized = fixture.jjfx(&["pool", "1"]);
    assert!(sized.status.success(), "{}", stderr(&sized));

    let pool = fixture.pool_json();
    assert_eq!(
        pool["agent"], "opencode",
        "sizing the Pool must keep the profile set before it existed"
    );
    assert_eq!(pool["size"], 1);
    assert_eq!(pool["workers"].as_array().map(Vec::len), Some(1));
}

/// Setting a profile on a Pool that already has capacity edits the document in
/// place: the size, the Workers and the creation time are untouched, so the
/// existing Pool lifecycle is unchanged.
#[test]
fn setting_a_profile_leaves_existing_capacity_alone() {
    let fixture = Fixture::new();
    let sized = fixture.jjfx(&["pool", "1"]);
    assert!(sized.status.success(), "{}", stderr(&sized));
    let before = fixture.pool_json();

    let changed = fixture.jjfx(&["pool", "profile", "codex"]);
    assert!(changed.status.success(), "{}", stderr(&changed));

    let after = fixture.pool_json();
    assert_eq!(after["agent"], "codex");
    assert_eq!(after["size"], before["size"]);
    assert_eq!(after["workers"], before["workers"]);
    assert_eq!(
        after["created_at"], before["created_at"],
        "an existing Pool document is edited, not replaced"
    );
}
