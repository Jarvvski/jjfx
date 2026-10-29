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

use std::env;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use jjfx::test_support::LIVENESS_TIMEOUT;
use rustix::process::{Pid, test_kill_process_group};
use serde_json::Value;
use tempfile::TempDir;

mod support;

use support::{HelperGuard, wait_for_file_content};

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

    /// The user-global `jjfx` config the binary under test reads, under the
    /// fixture's own `XDG_CONFIG_HOME`.
    fn write_config(&self, content: &str) {
        let directory = self.config_home.path().join("jjfx");
        fs::create_dir_all(&directory).expect("config directory");
        fs::write(directory.join("config.toml"), content).expect("config file");
    }

    /// Write one local Ticket under `.scratch/<feature>/issues/`.
    fn write_ticket(&self, feature: &str, file: &str, content: &str) {
        let directory = self.path().join(".scratch").join(feature).join("issues");
        fs::create_dir_all(&directory).expect("creating the Ticket directory");
        fs::write(directory.join(file), content).expect("writing the Ticket");
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

    /// Record `pid` as the live process group the busy `worker`'s Run leads.
    ///
    /// [`Fixture::write_pool_state`] records `pid: null`, which exercises the
    /// forced path only up to the point where there is a process to terminate.
    /// A forced removal that actually kills a Run needs the persisted PID to
    /// name a real process group - one this fixture started, never an arbitrary
    /// PID - so this rewrites that one field.
    fn record_worker_pid(&self, pool: &str, worker: &str, pid: i64) {
        let path = self.pool_state(pool).join(format!("{worker}.json"));
        let mut state: Value = serde_json::from_slice(&fs::read(&path).expect("Worker state"))
            .expect("Worker state JSON");
        state["pid"] = Value::from(pid);
        fs::write(
            &path,
            serde_json::to_vec_pretty(&state).expect("Worker state JSON"),
        )
        .expect("writing the Worker state with a live PID");
    }

    /// Rewrite one field of a Worker's state, mirroring [`Self::record_worker_pid`].
    fn record_worker_ticket(&self, pool: &str, worker: &str, ticket: &str) {
        let path = self.pool_state(pool).join(format!("{worker}.json"));
        let mut state: Value = serde_json::from_slice(&fs::read(&path).expect("Worker state"))
            .expect("Worker state JSON");
        state["ticket"] = Value::from(ticket);
        fs::write(
            &path,
            serde_json::to_vec_pretty(&state).expect("Worker state JSON"),
        )
        .expect("writing the Worker state with a claimed Ticket");
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
    /// there, so removing the Worker Pool would discard it.
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
    // Worker Pool's state directory and its Workers' Workspaces with the declaration.
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

/// Starts a real, harmless long-running process in a process group of its own
/// and records the group leader's PID, the way a background Run does.
///
/// Run as a helper subprocess by
/// [`destroy_force_terminates_a_live_worker_process_group`]: the test owns the
/// process it starts, so the forced removal can be driven end to end without
/// ever signalling a PID the test did not create.
#[test]
#[ignore]
fn destroy_live_process_helper() {
    let pid_path = env::var_os("JJFX_DESTROY_PID").expect("PID path");
    let mut child = Command::new("sh")
        .args(["-c", "trap 'exit 0' TERM; while :; do sleep 0.05; done"])
        .process_group(0)
        .spawn()
        .expect("spawn live Run");
    fs::write(pid_path, child.id().to_string()).expect("write live PID");
    let status = child.wait().expect("wait for destroyed Run");
    assert!(status.success(), "Run should handle TERM successfully");
}

/// `pool destroy --force` against a live busy Worker terminates the process
/// group that Worker's Run leads, and removes the Pool's state directory and
/// its declaration with it.
///
/// This is the path an operator reaches for when a Worker is stuck, and the
/// only one that discards work; the refusal side and the `pid: null` forced
/// path are covered elsewhere, so this is the one test that proves the kill end
/// to end. The process group is one this test started - the helper records its
/// own PID - so nothing here signals a PID the test does not own.
#[test]
fn destroy_force_terminates_a_live_worker_process_group() {
    let fixture = Fixture::new();
    fixture.declare(&[("bugs", &["worker-01"])]);
    fixture.write_pool_state("bugs", &[("worker-01", "busy", Some("eng-101"))]);
    fixture.provision_worker("worker-01");
    let pid_path = fixture.path().join("live-pid");
    let mut helper = Command::new(env::current_exe().expect("test executable"));
    helper
        .args(["--exact", "destroy_live_process_helper", "--ignored"])
        .env("JJFX_DESTROY_PID", &pid_path);
    let mut helper = HelperGuard::spawn(&mut helper, None, &[&pid_path]);
    let pid = wait_for_file_content(&pid_path)
        .trim()
        .parse::<i32>()
        .expect("numeric live PID");
    fixture.record_worker_pid("bugs", "worker-01", i64::from(pid));

    let output = fixture.jjfx(&["pool", "destroy", "bugs", "--force"]);

    assert!(
        output.status.success(),
        "a forced removal of a live Worker must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--force terminates"),
        "the forced path says what it kills: {stderr}"
    );
    // The process group the fixture started is gone...
    let pid = Pid::from_raw(pid).expect("live PID");
    let deadline = Instant::now() + LIVENESS_TIMEOUT;
    while test_kill_process_group(pid).is_ok() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        test_kill_process_group(pid).is_err(),
        "the busy Worker's process group should be terminated"
    );
    // ...and the helper that owned it exited cleanly on TERM, which bounds the
    // wait so a failed kill fails the test rather than hanging the suite.
    wait_for_helper_exit(&mut helper);
    let output = helper.wait_with_output();
    assert!(
        output.status.success(),
        "process owner failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The Pool's state directory and its declaration went with it.
    assert!(!fixture.pool_state("bugs").exists());
    assert!(!fixture.declares("bugs"));
}

/// Drive 4's findings 03 and 04: the declaration is announced before anything
/// moves, and a Pool that holds claimed Tickets refuses until `--force`, which
/// releases them rather than stranding them.
///
/// A claim belongs to the Pool that took it - the Ticket file records it and the
/// Pool's own queue is what would release it - so removing the Pool without
/// releasing the Ticket leaves it claimed by a queue that no longer exists, a
/// state no verb can undo.
#[test]
fn destroy_refuses_while_it_holds_claimed_tickets_and_force_releases_them() {
    let fixture = Fixture::new();
    fixture.write_config("[tickets]\nsource = \"local\"\n");
    fixture.declare(&[("bugs", &["worker-01"])]);
    fixture.write_pool_state("bugs", &[("worker-01", "idle", Some("bugs--01-thing"))]);
    fixture.record_worker_ticket("bugs", "worker-01", "bugs--01-thing");
    fixture.write_ticket("bugs", "01-thing.md", "# Thing\n\nStatus: claimed\n");
    let ticket = fixture.path().join(".scratch/bugs/issues/01-thing.md");

    // Without `--force`, the claimed Ticket refuses the removal by name.
    let refused = fixture.jjfx(&["pool", "destroy", "bugs"]);
    assert!(
        !refused.status.success(),
        "a claimed Ticket must refuse the removal"
    );
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("bugs--01-thing"), "{stderr}");
    assert!(stderr.contains("--force"), "{stderr}");
    assert!(fixture.declares("bugs"), "a refusal removes nothing");
    assert!(
        fs::read_to_string(&ticket)
            .expect("Ticket file")
            .contains("Status: claimed"),
        "a refusal releases nothing"
    );

    // `--force` removes the Pool and releases the claim with it.
    let forced = fixture.jjfx(&["pool", "destroy", "bugs", "--force"]);
    assert!(
        forced.status.success(),
        "a forced removal must succeed: {}",
        String::from_utf8_lossy(&forced.stderr)
    );
    let released = fs::read_to_string(&ticket).expect("Ticket file");
    assert!(
        released.contains("Status: ready-for-agent"),
        "the claim is released, not stranded: {released}"
    );
    // Released means dispatchable again, not merely rewritten: `ticket list`
    // reads it as ready.
    let listed = fixture.jjfx(&["ticket", "list"]);
    assert!(
        listed.status.success(),
        "ticket list failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(stdout.contains("bugs--01-thing"), "{stdout}");
    assert!(stdout.contains("ready"), "{stdout}");
    // And `resume` says the Pool is gone, which is a different recovery from
    // "nothing was ever queued" (drive 4, finding 04, fourth criterion).
    let resumed = fixture.jjfx(&["resume", "--pool", "bugs"]);
    assert!(
        !resumed.status.success(),
        "a destroyed Pool cannot be resumed"
    );
    let stderr = String::from_utf8_lossy(&resumed.stderr);
    assert!(stderr.contains("no Worker Pool named `bugs`"), "{stderr}");
    let stderr = String::from_utf8_lossy(&forced.stderr);
    assert!(
        stderr.contains("releases"),
        "the forced path says it releases the claim: {stderr}"
    );
    assert!(
        stderr.contains("tracked file"),
        "the declaration is announced as a tracked edit: {stderr}"
    );
    assert!(!fixture.declares("bugs"));
}
/// Waits for the helper to exit on its own, failing rather than hanging.
///
/// The helper blocks on the process group the removal is meant to terminate, so
/// a removal that never signals it would otherwise leave the test waiting
/// forever. Bounding the wait here is what turns that into a red test; the
/// armed [`HelperGuard`] still reaps the group and the helper on the way out.
fn wait_for_helper_exit(helper: &mut HelperGuard) {
    let deadline = Instant::now() + LIVENESS_TIMEOUT;
    while helper.is_running() {
        assert!(
            Instant::now() < deadline,
            "the helper should exit once its process group is terminated"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

/// Drive 4's finding 05: a Worker id is unique across the Repository, so a
/// Worker-scoped verb resolves the Worker's own Pool rather than assuming the
/// selected one.
///
/// `pool reset worker-x` for a Worker in `features` used to look the id up in
/// `default`, find nothing, and report a missing *state document* - which reads
/// as corruption rather than "wrong pool", and names neither the Pool it
/// searched nor the one the Worker is in.
#[test]
fn reset_resolves_the_pool_a_worker_is_in_and_names_the_pools_searched_otherwise() {
    let fixture = Fixture::new();
    fixture.declare(&[("bugs", &["worker-01"]), ("features", &["worker-02"])]);
    fixture.write_pool_state("features", &[("worker-02", "idle", None)]);

    let reset = fixture.jjfx(&["pool", "reset", "worker-02"]);
    assert!(
        reset.status.success(),
        "resetting a Worker in another Pool must work: {}",
        String::from_utf8_lossy(&reset.stderr)
    );
    let stderr = String::from_utf8_lossy(&reset.stderr);
    assert!(
        !stderr.contains("state is missing"),
        "the Worker exists; the message must not read as corruption: {stderr}"
    );
    assert!(stderr.contains("Reset worker-02"), "{stderr}");

    // A Worker in no Pool is refused by name, and the refusal names every Pool
    // that was searched, so "no such Worker" is distinguishable from "wrong
    // pool".
    let missing = fixture.jjfx(&["pool", "reset", "worker-99"]);
    assert!(!missing.status.success(), "an unknown Worker is refused");
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(stderr.contains("worker-99"), "{stderr}");
    assert!(
        stderr.contains("searched: default, bugs, features"),
        "the refusal must name the Pools it searched: {stderr}"
    );
}

/// The same resolution applies to `pool remove`, so a Worker is never reported
/// missing merely because it lives one lane over.
#[test]
fn remove_resolves_the_pool_a_worker_is_in() {
    let fixture = Fixture::new();
    fixture.declare(&[("bugs", &["worker-01"]), ("features", &["worker-02"])]);
    fixture.write_pool_state("features", &[("worker-02", "idle", None)]);

    let removed = fixture.jjfx(&["pool", "remove", "worker-02"]);
    assert!(
        removed.status.success(),
        "removing a Worker in another Pool must work: {}",
        String::from_utf8_lossy(&removed.stderr)
    );
    let stderr = String::from_utf8_lossy(&removed.stderr);
    assert!(stderr.contains("Removed worker-02"), "{stderr}");
    assert!(
        !stderr.contains("state is missing"),
        "the Worker exists; the message must not read as corruption: {stderr}"
    );
}

/// Drive 4's finding 07: `status` takes `--pool`, the selector `ticket list`
/// already offers, so the verb an operator watches Runs with can be narrowed to
/// one lane.
#[test]
fn status_narrows_to_one_pool_and_refuses_an_unknown_one() {
    let fixture = Fixture::new();
    fixture.declare(&[("bugs", &["worker-01"]), ("features", &["worker-02"])]);
    fixture.write_pool_state("bugs", &[("worker-01", "idle", None)]);
    fixture.write_pool_state("features", &[("worker-02", "idle", None)]);

    let narrowed = fixture.jjfx(&["status", "--pool", "features"]);
    assert!(
        narrowed.status.success(),
        "{}",
        String::from_utf8_lossy(&narrowed.stderr)
    );
    let stdout = String::from_utf8_lossy(&narrowed.stdout);
    assert!(stdout.contains("features"), "{stdout}");
    assert!(
        !stdout.contains("worker-01"),
        "the other lane is filtered out: {stdout}"
    );

    // An unknown name is refused by `resolve_pool`, naming the pools that exist.
    let unknown = fixture.jjfx(&["status", "--pool", "ghost"]);
    assert!(!unknown.status.success(), "an unknown pool is refused");
    let stderr = String::from_utf8_lossy(&unknown.stderr);
    assert!(stderr.contains("ghost"), "{stderr}");
}
