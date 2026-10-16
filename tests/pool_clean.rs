//! Scoping `Clean` to one Worker Pool: `jjfx clean --pool` (ticket 10).
//!
//! `Clean` removes Workers' Workspaces, so its blast radius is Workers rather
//! than trunk - the one maintenance verb that can destroy a neighbour lane's
//! work in progress. `--pool <name>` scopes the sweep to that pool's Workers'
//! Workspaces; the Repository-wide form stays the explicit "everything" and
//! stays the default, so nothing existing changes, and an unknown pool name is
//! refused loudly, naming the pools that exist.
#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

/// A throwaway repository plus the config home the binary under test reads.
///
/// Worker Workspaces are pinned under `<root>/ws` with `JJ_WS_DIR` so the
/// fixture owns every path it creates (a sibling `<repo>-workspaces` directory
/// would sit beside the `TempDir` and outlive it).
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
        // A base commit carrying a `main` bookmark, so the Worker Workspaces can
        // be created from something.
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

    fn worker_workspace(&self, worker: &str) -> PathBuf {
        self.ws_dir().join(worker)
    }

    fn pool_state(&self, pool: &str) -> PathBuf {
        self.path().join(".jj/pools").join(pool)
    }

    /// The jj config the binary under test reads.
    ///
    /// Passed as `JJ_CONFIG` rather than set with `jj config set --repo`: jj
    /// stores repo config under a directory derived from `XDG_CONFIG_HOME`,
    /// which this fixture points at its own config home, so a `--repo` write
    /// would land where the binary never looks.
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

    fn jj_in(&self, directory: &Path, args: &[&str]) -> Output {
        Command::new("jj")
            .args(args)
            .current_dir(directory)
            .env("JJ_CONFIG", self.jj_config_path())
            .output()
            .expect("jj should be installed")
    }

    fn jj_ok(&self, args: &[&str]) -> String {
        let output = self.jj_in(self.path(), args);
        assert!(
            output.status.success(),
            "jj {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// Run `jjfx` with `input` on stdin, so the clean confirmation is answered.
    fn jjfx_with_input(&self, args: &[&str], input: &str) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_jjfx"))
            .args(args)
            .current_dir(self.path())
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .env("JJ_CONFIG", self.jj_config_path())
            .env("JJ_WS_DIR", self.ws_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("jjfx should run");
        child
            .stdin
            .as_mut()
            .expect("piped stdin")
            .write_all(input.as_bytes())
            .expect("writing stdin");
        child.wait_with_output().expect("jjfx should finish")
    }

    /// Declare every named Pool the fixture holds, as `[pools.<name>]` sections.
    fn declare(&self, pools: &[(&str, &[&str])]) {
        let directory = self.path().join(".jjfx");
        fs::create_dir_all(&directory).expect("creating .jjfx");
        let text = pools
            .iter()
            .map(|(name, workers)| format!("[pools.{name}]\nsize = {}\n", workers.len()))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(directory.join("config.toml"), text).expect("writing the config");
    }

    /// Write one Pool's state directory and its Workers' documents.
    fn write_pool_state(&self, pool: &str, workers: &[&str]) {
        let directory = self.pool_state(pool);
        fs::create_dir_all(&directory).expect("creating the pool directory");
        let members = workers
            .iter()
            .map(|worker| format!("\"{worker}\""))
            .collect::<Vec<_>>()
            .join(", ");
        fs::write(
            directory.join("pool.json"),
            format!(
                "{{\"created_at\":\"2026-07-27T10:00:00Z\",\"gh_repo\":\"\",\"size\":{},\
                 \"workers\":[{members}]}}",
                workers.len()
            ),
        )
        .expect("writing the pool state");
    }

    /// Add a jj Workspace for `worker` at the Worker Workspace path the pool
    /// convention uses, based on `main`.
    fn provision_worker(&self, worker: &str) {
        let path = self.worker_workspace(worker);
        fs::create_dir_all(self.ws_dir()).expect("creating the workspace base");
        self.jj_ok(&[
            "workspace",
            "add",
            "--name",
            worker,
            "-r",
            "main",
            path.to_str().expect("workspace path is UTF-8"),
        ]);
    }
}

/// `clean --pool <name>` removes only that pool's non-default Workspaces,
/// leaving another lane's work in progress alone.
#[test]
fn clean_pool_scopes_the_sweep_to_one_lane() {
    let fixture = Fixture::new();
    fixture.declare(&[("bugs", &["worker-01"]), ("features", &["worker-02"])]);
    fixture.write_pool_state("bugs", &["worker-01"]);
    fixture.write_pool_state("features", &["worker-02"]);
    fixture.provision_worker("worker-01");
    fixture.provision_worker("worker-02");

    let output = fixture.jjfx_with_input(&["clean", "--pool", "bugs"], "y\n");

    assert!(
        output.status.success(),
        "a pool-scoped clean must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("worker-01"),
        "the plan names the scoped Worker: {stdout}"
    );
    assert!(
        !stdout.contains("worker-02"),
        "another lane's Worker is not swept: {stdout}"
    );
    // The scoped lane is gone; the other lane's Workspace is untouched.
    assert!(!fixture.worker_workspace("worker-01").exists());
    assert!(fixture.worker_workspace("worker-02").is_dir());
    // Clean removes Workspaces, not pool state: both documents remain.
    assert!(fixture.pool_state("bugs").join("pool.json").exists());
    assert!(fixture.pool_state("features").join("pool.json").exists());
}

/// The default - no `--pool` - stays Repository-wide, so every existing
/// invocation is unchanged and sweeps every non-default Workspace.
#[test]
fn clean_without_a_pool_sweeps_the_whole_repository() {
    let fixture = Fixture::new();
    fixture.declare(&[("bugs", &["worker-01"]), ("features", &["worker-02"])]);
    fixture.write_pool_state("bugs", &["worker-01"]);
    fixture.write_pool_state("features", &["worker-02"]);
    fixture.provision_worker("worker-01");
    fixture.provision_worker("worker-02");

    let output = fixture.jjfx_with_input(&["clean"], "y\n");

    assert!(
        output.status.success(),
        "a repository-wide clean must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.worker_workspace("worker-01").exists());
    assert!(!fixture.worker_workspace("worker-02").exists());
    // The default Workspace - the Repository root - is never removed.
    assert!(fixture.path().join("base.txt").exists());
}

/// An unknown pool name is a loud error naming the pools that exist, so a typo
/// cannot quietly sweep a lane that is not there.
#[test]
fn clean_refuses_an_unknown_pool_naming_the_pools_that_exist() {
    let fixture = Fixture::new();
    fixture.declare(&[("bugs", &["worker-01"]), ("features", &["worker-02"])]);
    fixture.provision_worker("worker-01");

    let output = fixture.jjfx_with_input(&["clean", "--pool", "ghost"], "y\n");

    assert!(
        !output.status.success(),
        "an unknown pool must be refused: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ghost"), "{stderr}");
    assert!(
        stderr.contains("bugs"),
        "the error names the pools: {stderr}"
    );
    assert!(
        stderr.contains("features"),
        "the error names the pools: {stderr}"
    );
    // Nothing was touched.
    assert!(fixture.worker_workspace("worker-01").is_dir());
}

/// A pool scope removes only its Workers' Workspaces: an Ad Hoc Workspace that
/// belongs to no pool is left for the Repository-wide sweep.
#[test]
fn clean_pool_leaves_a_workspace_that_belongs_to_no_pool() {
    let fixture = Fixture::new();
    fixture.declare(&[("bugs", &["worker-01"])]);
    fixture.write_pool_state("bugs", &["worker-01"]);
    fixture.provision_worker("worker-01");
    fixture.provision_worker("scratch");

    let output = fixture.jjfx_with_input(&["clean", "--pool", "bugs"], "y\n");

    assert!(
        output.status.success(),
        "a pool-scoped clean must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.worker_workspace("worker-01").exists());
    assert!(
        fixture.worker_workspace("scratch").is_dir(),
        "an Ad Hoc Workspace belongs to no pool and is not swept"
    );
}

/// A caller that never answers - stdin at EOF, as `clean </dev/null` gets - is
/// refused loudly and removes nothing, so "nothing happened" cannot be mistaken
/// for "nothing needed doing".
#[test]
fn clean_at_eof_refuses_loudly_and_removes_nothing() {
    let fixture = Fixture::new();
    fixture.declare(&[("bugs", &["worker-01"])]);
    fixture.write_pool_state("bugs", &["worker-01"]);
    fixture.provision_worker("worker-01");

    // An empty write leaves stdin at EOF: the answer never arrives.
    let output = fixture.jjfx_with_input(&["clean", "--pool", "bugs"], "");

    assert!(
        !output.status.success(),
        "a clean with no answer must fail: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no confirmation"),
        "the refusal says no confirmation was received: {stderr}"
    );
    assert!(
        fixture.worker_workspace("worker-01").is_dir(),
        "nothing is removed at EOF"
    );
}

/// An explicit `n` declines and exits 0, the contract `clean` already had: the
/// user chose not to act, which is not the same as never answering.
#[test]
fn clean_declined_with_n_removes_nothing_and_succeeds() {
    let fixture = Fixture::new();
    fixture.declare(&[("bugs", &["worker-01"])]);
    fixture.write_pool_state("bugs", &["worker-01"]);
    fixture.provision_worker("worker-01");

    let output = fixture.jjfx_with_input(&["clean", "--pool", "bugs"], "n\n");

    assert!(
        output.status.success(),
        "an explicit decline still succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fixture.worker_workspace("worker-01").is_dir(),
        "a declined clean removes nothing"
    );
}

/// The confirmation prompt is written to stderr, so stdout carries only the
/// plan's listing and `jjfx clean > out.txt` captures no prompt.
#[test]
fn clean_prompt_goes_to_stderr_not_stdout() {
    let fixture = Fixture::new();
    fixture.declare(&[("bugs", &["worker-01"])]);
    fixture.write_pool_state("bugs", &["worker-01"]);
    fixture.provision_worker("worker-01");

    let output = fixture.jjfx_with_input(&["clean", "--pool", "bugs"], "y\n");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Confirm?"),
        "the prompt is on stderr: {stderr}"
    );
    assert!(
        !stdout.contains("Confirm?"),
        "the prompt is not on stdout: {stdout}"
    );
}
