//! The capacity governor, driven end to end through the binary.
//!
//! A configured `[dispatch] max_concurrent` is a ceiling on how much is *in
//! flight*, counted across Dispatches, not a batch size. The fixtures here drive
//! more ready work than the limit and assert the limit holds - both when the
//! excess is in one batch and when a later Dispatch arrives while the earlier
//! Runs are still in flight - and that `status --json` makes the limit, the
//! running count and the held count readable as structure.
//!
//! Each case drives the real binary, because the point is the whole path: the
//! `[dispatch]` setting, the governor at the Dispatch seam, and the document a
//! frontend reads. The Pool's Workers are seeded as state rather than
//! provisioned, so a case exercises the governor without spending a Workspace.
//!
//! The Worker Pool is the other dial that bounds Dispatch, and the two gaps read
//! the same way: a Pool with no free Worker is a capacity gap, not a failure, so
//! `dispatch --all` and a named `dispatch` both report the Ticket as `waiting`.
#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

mod support;

/// A jj Repository with a seeded four-Worker `default` pool, a user config home
/// of its own, and a stub terminal on `PATH`.
struct Fixture {
    root: TempDir,
    config_home: TempDir,
    /// Holds the stub `kitten` binary; only its lifetime matters.
    _bin: TempDir,
    env_path: OsString,
}

impl Fixture {
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
        Self {
            root,
            config_home,
            _bin: bin,
            env_path,
        }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    /// The user-global jjfx config: the `[dispatch]` ceiling and a local Ticket
    /// source, so `--all` discovers the Tickets written by [`Self::write_ticket`].
    fn write_user_config(&self, content: &str) {
        let directory = self.config_home.path().join("jjfx");
        fs::create_dir_all(&directory).expect("config directory");
        fs::write(directory.join("config.toml"), content).expect("config file");
    }

    /// The repository-local jjfx config, `<repo>/.jjfx/config.toml` - where a
    /// Repository declares its own `[dispatch]` ceiling without touching the
    /// user's global config.
    fn write_repo_config(&self, content: &str) {
        let directory = self.path().join(".jjfx");
        fs::create_dir_all(&directory).expect(".jjfx directory");
        fs::write(directory.join("config.toml"), content).expect("repository config file");
    }

    /// Seeds `.jj/pools/default/` with `size` Workers, the first `busy` of which
    /// are recorded as running a Run - the in-flight Runs a later Dispatch must
    /// count against the cap.
    fn seed_pool(&self, size: usize, busy: usize) {
        let directory = self.path().join(".jj/pools/default");
        fs::create_dir_all(&directory).expect("pool directory");
        let workers = (1..=size)
            .map(|index| format!("\"worker-{index:02}\""))
            .collect::<Vec<_>>()
            .join(",");
        fs::write(
            directory.join("pool.json"),
            format!(
                r#"{{"size":{size},"gh_repo":"owner/repo","workers":[{workers}],"created_at":"2026-10-20T00:00:00Z","names":{{}}}}"#
            ),
        )
        .expect("pool state");
        for index in 1..=size {
            let running = index <= busy;
            let status = if running { "busy" } else { "idle" };
            let ticket = if running {
                format!("\"RUN-{index}\"")
            } else {
                "null".to_owned()
            };
            let started = if running {
                "\"2026-10-20T00:00:00Z\""
            } else {
                "null"
            };
            fs::write(
                directory.join(format!("worker-{index:02}.json")),
                format!(
                    r#"{{"status":"{status}","agent":"claude","ticket":{ticket},"pid":null,"started_at":{started},"completed_at":null,"log_file":null,"branch_name":null,"exit_code":null,"error":null}}"#
                ),
            )
            .expect("worker state");
        }
    }

    /// Writes a ready local Ticket, so `dispatch --all` has more work than a cap
    /// of two can hold.
    fn write_ticket(&self, file: &str, title: &str) {
        let directory = self.path().join(".scratch/widgets/issues");
        fs::create_dir_all(&directory).expect("issues directory");
        fs::write(
            directory.join(file),
            format!(
                "# {title}\n\nStatus: ready-for-agent\nType: task\nBlocked by: none\n\n## Problem\n\nDo it.\n"
            ),
        )
        .expect("Ticket file");
    }

    /// Runs `jjfx`, retrying only Dispatch's terminal refusal (see
    /// [`support::run_jjfx_tolerating_a_starved_terminal`]).
    fn jjfx(&self, args: &[&str]) -> Output {
        support::run_jjfx_tolerating_a_starved_terminal(|| {
            let mut command = Command::new(env!("CARGO_BIN_EXE_jjfx"));
            command
                .args(args)
                .current_dir(self.path())
                .env("XDG_CONFIG_HOME", self.config_home.path())
                .env("PATH", &self.env_path);
            command
        })
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn status_json(fixture: &Fixture) -> Value {
    let output = fixture.jjfx(&["status", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    serde_json::from_str(&stdout(&output)).expect("valid status JSON")
}

/// The `detail` of every `waiting` entry in a Dispatch batch, in order.
fn waiting_details(entries: &Value) -> Vec<String> {
    entries
        .as_array()
        .expect("an array of outcomes")
        .iter()
        .filter(|entry| entry["outcome"] == "waiting")
        .map(|entry| entry["detail"].as_str().expect("a reason").to_owned())
        .collect()
}

/// The fixture: a four-Worker Pool with two Runs already in flight, and three
/// ready Tickets - more than the cap of two can hold.
fn fixture() -> Fixture {
    let fixture = Fixture::new();
    fixture.write_user_config("[tickets]\nsource = \"local\"\n[dispatch]\nmax_concurrent = 2\n");
    fixture.seed_pool(4, 2);
    fixture.write_ticket("01-alpha.md", "Alpha");
    fixture.write_ticket("02-beta.md", "Beta");
    fixture.write_ticket("03-gamma.md", "Gamma");
    fixture
}

/// The criterion a governor exists for: more ready work than the limit, and the
/// limit holds. Two Runs are already in flight from an earlier Dispatch, so the
/// cap of two has no room and every ready Ticket is held back - even though two
/// Workers sit idle. That is the governor, not pool capacity.
#[test]
fn a_dispatch_holds_more_ready_work_back_than_the_limit_allows() {
    let fixture = fixture();

    let output = fixture.jjfx(&["dispatch", "--all", "--json"]);
    assert!(
        output.status.success(),
        "{}\n{}",
        stdout(&output),
        stderr(&output)
    );
    let entries: Value = serde_json::from_str(stdout(&output).trim()).expect("valid JSON");
    let entries = entries.as_array().expect("an array of outcomes");

    let held = entries
        .iter()
        .filter(|entry| entry["outcome"] == "waiting")
        .collect::<Vec<_>>();
    assert_eq!(
        held.len(),
        3,
        "all three ready Tickets must be held: {}",
        stdout(&output)
    );
    assert!(
        entries.iter().all(|entry| entry["outcome"] != "dispatched"),
        "nothing may fire while the cap is spent: {}",
        stdout(&output)
    );
    // The reason is the useful part: the held entry names the setting and what
    // has to change, not just a count.
    for entry in &held {
        let detail = entry["detail"].as_str().expect("a reason");
        assert!(
            detail.contains("max_concurrent = 2"),
            "the reason must name the binding limit: {detail}"
        );
        assert!(
            detail.contains("must finish"),
            "the reason must say why nothing starts: {detail}"
        );
    }

    // The two idle Workers are untouched: the governor held the work, the Pool
    // never ran out of capacity.
    let value = status_json(&fixture);
    let idle = value["workers"]
        .as_array()
        .expect("workers")
        .iter()
        .filter(|worker| worker["status"] == "idle")
        .count();
    assert_eq!(idle, 2, "the free Workers stay idle: {value}");
}

/// The limit is visible as structure: `status --json` reports the cap, how many
/// Runs are in flight, and how many Tickets are held.
#[test]
fn status_json_reports_the_limit_the_running_count_and_the_held_count() {
    let fixture = fixture();
    // Hold a batch first, so `held` has something to report.
    let output = fixture.jjfx(&["dispatch", "--all", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));

    let value = status_json(&fixture);

    assert_eq!(value["governor"]["limit"], 2, "{value}");
    assert_eq!(value["governor"]["running"], 2, "{value}");
    assert_eq!(value["governor"]["held"], 3, "{value}");
    // The held Tickets are the waiting set, tagged with the pool that held them.
    assert_eq!(
        value["waiting"]
            .as_array()
            .expect("waiting")
            .iter()
            .map(|entry| entry["ticket"].as_str().expect("ticket"))
            .collect::<Vec<_>>(),
        ["widgets--01-alpha", "widgets--02-beta", "widgets--03-gamma"]
    );

    // The prose carries the same fact, so a human reads the ceiling too.
    let prose = fixture.jjfx(&["status"]);
    assert!(prose.status.success(), "{}", stderr(&prose));
    let text = stdout(&prose);
    assert!(
        text.contains("Concurrency: 2 Run(s) in flight (dispatch.max_concurrent = 2)"),
        "{text}"
    );
    assert!(text.contains("3 held back"), "{text}");
}

/// An uncapped Repository reports the absence of a limit rather than a zero: no
/// ceiling is a real answer, and a `0` would read as "nothing may ever run".
#[test]
fn status_json_reports_no_limit_when_none_is_configured() {
    let fixture = Fixture::new();
    fixture.write_user_config("[tickets]\nsource = \"local\"\n");
    fixture.seed_pool(2, 1);

    let value = status_json(&fixture);

    assert_eq!(value["governor"]["limit"], Value::Null, "{value}");
    assert_eq!(value["governor"]["running"], 1, "{value}");
    assert_eq!(value["governor"]["held"], 0, "{value}");
}

/// The governor counts across Dispatches: a second Dispatch that arrives while
/// the earlier Runs are still in flight fires nothing, rather than treating each
/// Dispatch as a fresh batch of `limit`.
#[test]
fn the_limit_survives_across_dispatches() {
    let fixture = fixture();

    let first = fixture.jjfx(&["dispatch", "--all", "--json"]);
    assert!(first.status.success(), "{}", stderr(&first));
    let second = fixture.jjfx(&["dispatch", "--all", "--json"]);
    assert!(second.status.success(), "{}", stderr(&second));

    let first_entries: Value = serde_json::from_str(stdout(&first).trim()).expect("valid JSON");
    let entries: Value = serde_json::from_str(stdout(&second).trim()).expect("valid JSON");
    let held = entries
        .as_array()
        .expect("an array")
        .iter()
        .filter(|entry| entry["outcome"] == "waiting")
        .count();
    assert_eq!(
        held,
        3,
        "the second Dispatch must hold the same work: {}",
        stdout(&second)
    );

    // The two Dispatches are the same situation reached twice, and the two
    // messages must agree about it: both name the count that actually bound the
    // batch - the two Runs the governor reports in flight - not the pre-launch
    // count of whichever Dispatch happened to run first.
    let first_details = waiting_details(&first_entries);
    let second_details = waiting_details(&entries);
    assert_eq!(
        first_details, second_details,
        "the two Dispatches must agree about the same state"
    );
    assert!(
        first_details
            .iter()
            .all(|detail| detail.contains("2 Run(s) in flight")),
        "{first_details:?}"
    );

    // Nothing was launched by either Dispatch: the two in-flight Runs are still
    // the only ones, and the two free Workers are still free.
    let value = status_json(&fixture);
    assert_eq!(value["governor"]["running"], 2, "{value}");
    assert_eq!(value["governor"]["held"], 3, "{value}");
}

/// The cap detail names the count that actually bound the batch. A first
/// Dispatch has nothing in flight, so the pre-launch count is `0`; it fires its
/// prefix and the cap holds the rest, so the detail must name the count the
/// governor reports - the two Runs now in flight - not a pre-launch zero.
#[test]
fn a_capped_dispatch_names_the_count_that_bound_the_batch() {
    let fixture = Fixture::new();
    fixture.write_user_config("[tickets]\nsource = \"local\"\n[dispatch]\nmax_concurrent = 2\n");
    // Four idle Workers, so the cap is the binding dial, not pool capacity.
    fixture.seed_pool(4, 0);
    fixture.write_ticket("01-alpha.md", "Alpha");
    fixture.write_ticket("02-beta.md", "Beta");
    fixture.write_ticket("03-gamma.md", "Gamma");

    let output = fixture.jjfx(&["dispatch", "--all", "--json"]);
    assert!(
        output.status.success(),
        "{}\n{}",
        stdout(&output),
        stderr(&output)
    );
    let entries: Value = serde_json::from_str(stdout(&output).trim()).expect("valid JSON");

    // The cap fires two and holds the third, so one reason stands for the gap.
    let details = waiting_details(&entries);
    assert_eq!(details.len(), 1, "{}", stdout(&output));
    assert!(
        details[0].contains("2 Run(s) in flight"),
        "the count that bound the batch is the two it fired: {}",
        details[0]
    );
    assert!(
        !details[0].contains("0 Run(s) in flight"),
        "a pre-launch zero is the bug: {}",
        details[0]
    );
}

/// A repository may declare its own `[dispatch] max_concurrent`, and it binds
/// with no user-global cap at all - the case the governor was unreachable for,
/// because the cap's only home was the user's global config. This is the
/// repository-owned ceiling the finding asked for, read without touching the
/// user's config.
#[test]
fn a_repository_dispatch_ceiling_binds_without_touching_the_user_global_config() {
    let fixture = Fixture::new();
    // The user-global config sets no ceiling - the repository owns this one.
    fixture.write_user_config("[tickets]\nsource = \"local\"\n");
    fixture.write_repo_config("[dispatch]\nmax_concurrent = 2\n");
    // Four Workers with two Runs already in flight: the cap, not capacity, is
    // what must hold the ready work.
    fixture.seed_pool(4, 2);
    fixture.write_ticket("01-alpha.md", "Alpha");
    fixture.write_ticket("02-beta.md", "Beta");
    fixture.write_ticket("03-gamma.md", "Gamma");

    let output = fixture.jjfx(&["dispatch", "--all", "--json"]);
    assert!(
        output.status.success(),
        "{}\n{}",
        stdout(&output),
        stderr(&output)
    );
    let entries: Value = serde_json::from_str(stdout(&output).trim()).expect("valid JSON");
    let held = waiting_details(&entries);
    assert_eq!(held.len(), 3, "all three must be held: {}", stdout(&output));
    for detail in &held {
        assert!(
            detail.contains("max_concurrent = 2"),
            "the reason must name the repository ceiling: {detail}"
        );
    }

    // The binding limit is visible where the pool is read.
    let value = status_json(&fixture);
    assert_eq!(value["governor"]["limit"], 2, "{value}");
    assert_eq!(value["governor"]["running"], 2, "{value}");
    assert_eq!(value["governor"]["held"], 3, "{value}");
}

/// A repository ceiling only *tightens* the user-global one: with both set, the
/// smaller binds.
#[test]
fn a_repository_dispatch_ceiling_tightens_the_user_global_one() {
    let fixture = Fixture::new();
    fixture.write_user_config("[tickets]\nsource = \"local\"\n[dispatch]\nmax_concurrent = 4\n");
    fixture.write_repo_config("[dispatch]\nmax_concurrent = 1\n");
    // Four idle Workers, so the ceiling is the binding dial, not capacity.
    fixture.seed_pool(4, 0);
    fixture.write_ticket("01-alpha.md", "Alpha");
    fixture.write_ticket("02-beta.md", "Beta");
    fixture.write_ticket("03-gamma.md", "Gamma");

    let output = fixture.jjfx(&["dispatch", "--all", "--json"]);
    assert!(
        output.status.success(),
        "{}\n{}",
        stdout(&output),
        stderr(&output)
    );
    let entries: Value = serde_json::from_str(stdout(&output).trim()).expect("valid JSON");
    let held = waiting_details(&entries);
    assert_eq!(
        held.len(),
        2,
        "a ceiling of one fires one and holds two: {}",
        stdout(&output)
    );
    for detail in &held {
        assert!(
            detail.contains("max_concurrent = 1"),
            "the smaller repository ceiling binds: {detail}"
        );
    }

    let value = status_json(&fixture);
    assert_eq!(value["governor"]["limit"], 1, "{value}");
}

/// A repository ceiling may never *widen* the user-global one: a repository that
/// declares a larger value leaves the user-global limit binding (ADR 0014 - a
/// repository-local declaration selects, never grants).
#[test]
fn a_repository_dispatch_ceiling_never_widens_the_user_global_one() {
    let fixture = Fixture::new();
    fixture.write_user_config("[tickets]\nsource = \"local\"\n[dispatch]\nmax_concurrent = 2\n");
    fixture.write_repo_config("[dispatch]\nmax_concurrent = 8\n");
    fixture.seed_pool(2, 1);

    let value = status_json(&fixture);
    assert_eq!(
        value["governor"]["limit"], 2,
        "the user-global ceiling still binds: {value}"
    );
}

/// No free Worker is a capacity gap, not a failure, and the two ways to reach it
/// agree: `dispatch --all` and a named `dispatch` report the same Ticket as
/// `waiting`, with the same reason.
#[test]
fn a_pool_with_no_free_worker_is_reported_as_waiting_by_all_and_by_a_named_dispatch() {
    let fixture = Fixture::new();
    // Uncapped, so pool capacity is the only dial that can hold the Ticket.
    fixture.write_user_config("[tickets]\nsource = \"local\"\n");
    // Both Workers are busy, so there is no free Worker to run the Ticket.
    fixture.seed_pool(2, 2);
    fixture.write_ticket("01-alpha.md", "Alpha");

    let all = fixture.jjfx(&["dispatch", "--all", "--json"]);
    assert!(all.status.success(), "{}\n{}", stdout(&all), stderr(&all));
    let all_json: Value = serde_json::from_str(stdout(&all).trim()).expect("valid JSON");
    assert_eq!(all_json[0]["outcome"], "waiting", "{all_json}");
    assert!(all_json[0]["worker"].is_null(), "{all_json}");

    let named = fixture.jjfx(&["dispatch", "--json", "widgets--01-alpha"]);
    assert!(
        named.status.success(),
        "{}\n{}",
        stdout(&named),
        stderr(&named)
    );
    let named_json: Value = serde_json::from_str(stdout(&named).trim()).expect("valid JSON");
    assert_eq!(named_json[0]["outcome"], "waiting", "{named_json}");
    assert!(named_json[0]["worker"].is_null(), "{named_json}");

    // One capacity gap, reported the same way by both paths.
    assert_eq!(
        all_json[0]["ticket"], named_json[0]["ticket"],
        "the same Ticket: all {all_json} named {named_json}"
    );
    assert_eq!(
        all_json[0]["detail"], named_json[0]["detail"],
        "the same reason: all {all_json} named {named_json}"
    );
    assert!(
        all_json[0]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("no idle Worker")),
        "{all_json}"
    );
}
