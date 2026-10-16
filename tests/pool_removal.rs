//! Removing a named Worker Pool: `jjfx pool destroy` (ticket 05).
//!
//! A Repository holds several named Worker Pools (ADR 0012), and removing one
//! lane while another keeps working is the thing the single-pool world never had
//! to answer. These tests drive the real binary against a real jj repo to prove
//! the decisions: removal **drains** (a busy Worker is refused, by name), it
//! **reports before it discards** (each Worker's branch, and whether that branch
//! is pushed or merged), it refuses undelivered work unless `--force`, and it
//! touches **only** the removed Pool's Workers' Workspaces - never another
//! lane's, and never the whole Repository the way `Clean` does.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
        // A base commit carrying a `main` bookmark, so trunk resolves and the
        // Worker Workspaces can be measured against something.
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

    /// Whether `[pools.<name>]` is still declared.
    fn declares(&self, pool: &str) -> bool {
        fs::read_to_string(self.path().join(".jjfx/config.toml"))
            .map(|text| text.contains(&format!("[pools.{pool}]")))
            .unwrap_or(false)
    }

    /// Write one Pool's state directory and its Workers' documents.
    fn write_pool_state(&self, pool: &str, workers: &[(&str, &str, Option<&str>)]) {
        let directory = self.pool_state(pool);
        fs::create_dir_all(&directory).expect("creating the pool directory");
        let members = workers
            .iter()
            .map(|(worker, _, _)| format!("\"{worker}\""))
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
        for (worker, status, branch) in workers {
            let branch = branch.map_or_else(|| "null".to_owned(), |name| format!("\"{name}\""));
            fs::write(
                directory.join(format!("{worker}.json")),
                format!(
                    "{{\"agent\":\"claude\",\"branch_name\":{branch},\"completed_at\":null,\
                     \"error\":null,\"exit_code\":null,\"log_file\":null,\"pid\":null,\
                     \"started_at\":null,\"status\":\"{status}\",\"ticket\":null}}"
                ),
            )
            .expect("writing the worker state");
        }
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

    /// Leave an unpushed commit in `worker`'s Workspace: work that exists only
    /// there, so removing the Pool would discard it.
    fn leave_local_work(&self, worker: &str) {
        let directory = self.worker_workspace(worker);
        fs::write(directory.join("work.txt"), "work\n").expect("worker edit");
        let output = self.jj_in(&directory, &["commit", "-m", "worker work"]);
        assert!(
            output.status.success(),
            "jj commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// A drain, not a wait: a busy Worker is refused by name, and nothing moves.
#[test]
fn destroy_refuses_a_busy_pool_and_names_the_worker() {
    let fixture = Fixture::new();
    fixture.declare(&[("bugs", &["worker-01", "worker-02"])]);
    fixture.write_pool_state(
        "bugs",
        &[
            ("worker-01", "busy", Some("eng-101")),
            ("worker-02", "idle", None),
        ],
    );

    let output = fixture.jjfx(&["pool", "destroy", "bugs"]);

    assert!(
        !output.status.success(),
        "a busy pool must be refused: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("worker-01"), "{stderr}");
    assert!(stderr.contains("busy"), "{stderr}");
    assert!(
        !stderr.contains("worker-02"),
        "only the busy Worker is named: {stderr}"
    );
    // The refusal is a drain: nothing was touched.
    assert!(fixture.pool_state("bugs").join("pool.json").exists());
    assert!(fixture.declares("bugs"));
}

/// Removal reports each Worker's branch and whether it is pushed or merged, and
/// refuses undelivered work - `--force` is the only path that discards it.
#[test]
fn destroy_reports_each_branch_and_refuses_undelivered_work_unless_forced() {
    let fixture = Fixture::new();
    fixture.declare(&[("bugs", &["worker-01"])]);
    fixture.write_pool_state("bugs", &[("worker-01", "idle", Some("eng-101"))]);
    fixture.provision_worker("worker-01");
    fixture.leave_local_work("worker-01");

    let output = fixture.jjfx(&["pool", "destroy", "bugs"]);

    assert!(
        !output.status.success(),
        "undelivered work must be refused: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("worker-01"), "{stderr}");
    assert!(stderr.contains("eng-101"), "{stderr}");
    assert!(
        stderr.contains("dirty"),
        "the branch's state is reported: {stderr}"
    );
    assert!(fixture.worker_workspace("worker-01").is_dir());
    assert!(fixture.pool_state("bugs").join("pool.json").exists());
    assert!(fixture.declares("bugs"));

    // `--force` overrides the drain, says what it discards, and removes the
    // Pool's state directory and its Workers' Workspaces with the declaration.
    let output = fixture.jjfx(&["pool", "destroy", "bugs", "--force"]);
    assert!(
        output.status.success(),
        "a forced removal must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("eng-101"),
        "the report names the branch: {stderr}"
    );
    assert!(
        stderr.contains("--force discards"),
        "the forced path says what it loses: {stderr}"
    );
    assert!(!fixture.worker_workspace("worker-01").exists());
    assert!(!fixture.pool_state("bugs").exists());
    assert!(!fixture.declares("bugs"));
}

/// A Pool whose Workers have all delivered is removed with no `--force`.
#[test]
fn destroy_removes_a_pool_whose_workers_have_delivered() {
    let fixture = Fixture::new();
    fixture.declare(&[("bugs", &["worker-01"])]);
    fixture.write_pool_state("bugs", &[("worker-01", "idle", None)]);
    fixture.provision_worker("worker-01");

    let output = fixture.jjfx(&["pool", "destroy", "bugs"]);

    assert!(
        output.status.success(),
        "a clean pool needs no --force: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!fixture.worker_workspace("worker-01").exists());
    assert!(!fixture.pool_state("bugs").exists());
    assert!(!fixture.declares("bugs"));
}

/// Unlike `Clean`, which sweeps every non-default Workspace in the Repository,
/// removing a Pool touches only its own Workers - another lane keeps working.
#[test]
fn destroy_touches_only_the_removed_pools_workers() {
    let fixture = Fixture::new();
    fixture.declare(&[("bugs", &["worker-01"]), ("features", &["worker-02"])]);
    fixture.write_pool_state("bugs", &[("worker-01", "idle", None)]);
    fixture.write_pool_state("features", &[("worker-02", "idle", Some("eng-202"))]);
    fixture.provision_worker("worker-01");
    fixture.provision_worker("worker-02");
    fixture.leave_local_work("worker-02");

    let output = fixture.jjfx(&["pool", "destroy", "bugs"]);

    assert!(
        output.status.success(),
        "removing a clean lane must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The removed lane is gone: its Workspace and its state directory.
    assert!(!fixture.worker_workspace("worker-01").exists());
    assert!(!fixture.pool_state("bugs").exists());
    assert!(!fixture.declares("bugs"));
    // The other lane is untouched - its Workspace, its state and its work.
    assert!(fixture.worker_workspace("worker-02").is_dir());
    assert!(
        fixture
            .worker_workspace("worker-02")
            .join("work.txt")
            .exists()
    );
    assert!(fixture.pool_state("features").join("pool.json").exists());
    assert!(fixture.declares("features"));
    // And the Repository itself is still there.
    assert!(fixture.path().join(".jj").is_dir());
}
