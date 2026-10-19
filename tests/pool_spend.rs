//! A pool's spend cap and the Repository ceiling both bind, and the refusal
//! names the one that ran out.
//!
//! Each case drives the real binary, because the point is what a Dispatch does
//! when a limit has no room: a discovered (`--all`) batch is held back visibly in
//! `waiting`, while an explicit Dispatch refuses loudly - the owner asked for
//! that specific Ticket. The refusal names the dial to turn: the pool when its
//! own cap is the tighter constraint, the Repository when the ceiling is, because
//! naming a pool there would send the owner to the wrong knob.
#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

mod support;

/// A jj Repository with one ready local Ticket, a user config home of its own,
/// and a stub terminal so Dispatch does not depend on the ambient one.
struct SpendFixture {
    root: TempDir,
    config_home: TempDir,
    /// The stub `kitten`, held so it outlives the `PATH` the binary is given.
    _bin: TempDir,
    env_path: OsString,
}

impl SpendFixture {
    /// `user_config` is the user-global jjfx config; every fixture selects the
    /// local Ticket source so discovery needs no tracker.
    fn new(user_config: &str) -> Self {
        let root = TempDir::new().expect("temporary repository");
        let config_home = TempDir::new().expect("temporary config home");
        let bin = TempDir::new().expect("temporary bin");
        let env_path = support::stub_terminal(bin.path());
        let fixture = Self {
            root,
            config_home,
            _bin: bin,
            env_path,
        };
        fixture.jj(&["git", "init", "--no-colocate"]);
        fixture.write_ticket(
            "widgets",
            "01-ship-the-widget.md",
            "# Ship the widget\n\nStatus: ready-for-agent\n",
        );
        fixture.write_user_config(user_config);
        // The delivery identity is passed as `JJ_CONFIG` rather than set with
        // `jj config set --repo`: jj stores repo config under a directory derived
        // from `XDG_CONFIG_HOME`, which this fixture points at its own config
        // home, so a `--repo` write would land where the binary never looks.
        fs::write(
            fixture.jj_config_path(),
            "user.name = \"Test User\"\nuser.email = \"test@example.com\"\n",
        )
        .expect("jj config file");
        fixture
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn jj_config_path(&self) -> PathBuf {
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

    /// The user-global jjfx config, so the binary reads a known home. The local
    /// Ticket source is always selected.
    fn write_user_config(&self, content: &str) {
        let directory = self.config_home.path().join("jjfx");
        fs::create_dir_all(&directory).expect("config directory");
        fs::write(
            directory.join("config.toml"),
            format!("[tickets]\nsource = \"local\"\n{content}"),
        )
        .expect("config file");
    }

    /// The Repository's own `.jjfx/config.toml`, which declares the pools.
    fn write_repo_config(&self, content: &str) {
        let directory = self.path().join(".jjfx");
        fs::create_dir_all(&directory).expect("repository config directory");
        fs::write(directory.join("config.toml"), content).expect("repository config file");
    }

    /// Sizes `default`, so the pool's state directory exists.
    fn with_pool(&self, size: usize) {
        let output = self.jjfx(&["pool", &size.to_string()]);
        assert!(output.status.success(), "{}", stderr(&output));
    }

    /// Replaces the stub terminal with one that fails, so the occupancy query
    /// fails closed the way an unreachable terminal does.
    fn break_terminal(&self) {
        use std::os::unix::fs::PermissionsExt;
        let kitten = self._bin.path().join("kitten");
        fs::write(&kitten, "#!/bin/sh\nexit 1\n").expect("failing terminal stub");
        fs::set_permissions(&kitten, fs::Permissions::from_mode(0o755)).expect("executable stub");
    }

    /// Seeds `default`'s durable Run records with one Run costing `micro_usd`.
    ///
    /// The cap is totalled from these records, so this is what puts a pool over
    /// its cap without launching anything.
    fn seed_spend(&self, micro_usd: u64) {
        let directory = self.path().join(".jj/pools/default");
        fs::create_dir_all(&directory).expect("pool directory");
        fs::write(
            directory.join("runs.jsonl"),
            format!(
                "{}\n",
                serde_json::json!({
                    "run_id": "worker-01@1",
                    "worker": "worker-01",
                    "cost_micro_usd": micro_usd,
                    "conclusion": "succeeded",
                    "completed_at": "2026-10-15T09:00:00Z",
                })
            ),
        )
        .expect("run records");
    }

    /// The Tickets `default`'s waiting set holds.
    fn waiting(&self) -> Vec<String> {
        let path = self.path().join(".jj/pools/default/waiting.json");
        let Ok(text) = fs::read_to_string(&path) else {
            return Vec::new();
        };
        let value: Value = serde_json::from_str(&text).expect("valid waiting JSON");
        value["tickets"]
            .as_array()
            .expect("tickets array")
            .iter()
            .map(|ticket| ticket.as_str().expect("ticket id").to_owned())
            .collect()
    }

    /// Runs `jjfx`, retrying a Dispatch that failed closed because the stub
    /// terminal was starved past its deadline under load - see
    /// [`support::run_jjfx_tolerating_a_starved_terminal`].
    fn jjfx(&self, args: &[&str]) -> Output {
        support::run_jjfx_tolerating_a_starved_terminal(|| {
            let mut command = Command::new(env!("CARGO_BIN_EXE_jjfx"));
            command
                .args(args)
                .current_dir(self.path())
                .env("XDG_CONFIG_HOME", self.config_home.path())
                .env("JJ_CONFIG", self.jj_config_path())
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

/// The Repository ceiling has run out while `default` still has room: the
/// discovered batch is held, and the refusal names the Repository - the dial the
/// owner turns - not the pool.
#[test]
fn a_discovered_batch_is_held_when_the_ceiling_has_run_out() {
    let fixture = SpendFixture::new("[spend]\ncap_usd = 5.0\n");
    fixture.write_repo_config("[pools.default]\ncap_usd = 100.0\n");
    fixture.with_pool(1);
    fixture.seed_spend(6_000_000);

    let output = fixture.jjfx(&["dispatch", "--all", "--json"]);
    assert!(
        output.status.success(),
        "a held batch is a deliberate outcome, not a failure:\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let json = stdout(&output);
    assert!(json.contains("\"outcome\": \"waiting\""), "{json}");
    assert!(json.contains("Repository"), "the ceiling is named: {json}");
    assert!(!json.contains("pool `default`"), "not the pool: {json}");
    assert_eq!(fixture.waiting(), ["widgets--01-ship-the-widget"]);
}

/// `default`'s own cap has run out: the batch is held, and the refusal names the
/// pool, whose setting is the dial that unblocks it.
#[test]
fn a_discovered_batch_is_held_when_the_pool_cap_has_run_out() {
    let fixture = SpendFixture::new("[spend]\ncap_usd = 100.0\n");
    fixture.write_repo_config("[pools.default]\ncap_usd = 5.0\n");
    fixture.with_pool(1);
    fixture.seed_spend(6_000_000);

    let output = fixture.jjfx(&["dispatch", "--all", "--json"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let json = stdout(&output);
    assert!(json.contains("\"outcome\": \"waiting\""), "{json}");
    assert!(json.contains("pool `default`"), "{json}");
    assert!(json.contains("pools.default.cap_usd"), "{json}");
    assert_eq!(fixture.waiting(), ["widgets--01-ship-the-widget"]);
}

/// An explicit Dispatch refuses loudly rather than holding: the owner named the
/// Ticket, so a silent `waiting` would be the wrong answer.
#[test]
fn an_explicit_dispatch_refuses_loudly_when_a_cap_has_run_out() {
    let fixture = SpendFixture::new("[spend]\ncap_usd = 100.0\n");
    fixture.write_repo_config("[pools.default]\ncap_usd = 5.0\n");
    fixture.with_pool(1);
    fixture.seed_spend(6_000_000);

    let output = fixture.jjfx(&[
        "dispatch",
        "widgets--01-ship-the-widget",
        "--no-orchestrate",
    ]);
    assert!(
        !output.status.success(),
        "an explicit Dispatch must refuse:\nstdout:\n{}",
        stdout(&output)
    );
    let error = stderr(&output);
    assert!(error.contains("pool `default`"), "{error}");
    assert!(error.contains("pools.default.cap_usd"), "{error}");
    // Nothing was held: the refusal is loud, not a quiet waiting entry.
    assert!(fixture.waiting().is_empty());
}

/// `--budget` is a cap inside the caps: it composes by the same rule and never
/// rescues a limit that has already run out.
#[test]
fn a_budget_does_not_rescue_a_fired_cap() {
    let fixture = SpendFixture::new("[spend]\ncap_usd = 100.0\n");
    fixture.write_repo_config("[pools.default]\ncap_usd = 5.0\n");
    fixture.with_pool(1);
    fixture.seed_spend(6_000_000);

    let output = fixture.jjfx(&[
        "dispatch",
        "widgets--01-ship-the-widget",
        "--no-orchestrate",
        "--budget",
        "1",
    ]);
    assert!(
        !output.status.success(),
        "a budget must not bypass a fired cap:\nstdout:\n{}",
        stdout(&output)
    );
    assert!(
        stderr(&output).contains("pool `default`"),
        "{}",
        stderr(&output)
    );
}

/// `--budget` is a per-Dispatch limit inside the caps: with every cap having
/// room it neither refuses nor is refused - it bounds the Run and nothing more.
#[test]
fn a_budget_inside_roomy_caps_proceeds() {
    let fixture = SpendFixture::new("[spend]\ncap_usd = 100.0\n");
    fixture.write_repo_config("[pools.default]\ncap_usd = 100.0\n");
    fixture.with_pool(1);
    fixture.seed_spend(6_000_000);

    let output = fixture.jjfx(&[
        "dispatch",
        "widgets--01-ship-the-widget",
        "--no-orchestrate",
        "--budget",
        "1",
        "--json",
    ]);
    // The budget never refuses on its own: the Ticket is attempted, not held.
    let json = stdout(&output);
    assert!(!json.contains("\"outcome\": \"waiting\""), "{json}");
    assert!(
        !stderr(&output).contains("spend cap"),
        "{}",
        stderr(&output)
    );
}

/// `on_exceed = warn` stays per-level: a warning pool cap reports the overage
/// and Dispatch proceeds, while a blocking ceiling still refuses.
#[test]
fn a_warning_pool_cap_reports_and_proceeds() {
    let fixture = SpendFixture::new("[spend]\ncap_usd = 100.0\n");
    fixture.write_repo_config("[pools.default]\ncap_usd = 5.0\non_exceed = \"warn\"\n");
    fixture.with_pool(1);
    fixture.seed_spend(6_000_000);

    let output = fixture.jjfx(&["dispatch", "--all", "--json"]);
    let error = stderr(&output);
    assert!(error.contains("dispatching anyway"), "{error}");
    assert!(error.contains("pool `default`"), "{error}");
    // Warn mode proceeds rather than holding: nothing is in `waiting`.
    assert!(fixture.waiting().is_empty());
}

/// Holding a batch back reserves no Worker, so it needs no occupancy query: an
/// unreachable terminal must not mask the spend hold it is about to report.
#[test]
fn a_held_batch_does_not_need_the_terminal() {
    let fixture = SpendFixture::new("[spend]\ncap_usd = 5.0\n");
    fixture.write_repo_config("[pools.default]\ncap_usd = 100.0\n");
    fixture.with_pool(1);
    fixture.seed_spend(6_000_000);
    fixture.break_terminal();

    let output = fixture.jjfx(&["dispatch", "--all", "--json"]);
    assert!(
        output.status.success(),
        "a held batch must not fail on the terminal:\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(fixture.waiting(), ["widgets--01-ship-the-widget"]);
}

/// The blocking ceiling outranks a warning pool cap: every limit has to have
/// room for the Dispatch to proceed.
#[test]
fn a_blocking_ceiling_outranks_a_warning_pool_cap() {
    let fixture = SpendFixture::new("[spend]\ncap_usd = 5.0\n");
    fixture.write_repo_config("[pools.default]\ncap_usd = 100.0\non_exceed = \"warn\"\n");
    fixture.with_pool(1);
    fixture.seed_spend(6_000_000);

    let output = fixture.jjfx(&["dispatch", "--all", "--json"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let json = stdout(&output);
    assert!(json.contains("\"outcome\": \"waiting\""), "{json}");
    assert!(json.contains("Repository"), "{json}");
    assert_eq!(fixture.waiting(), ["widgets--01-ship-the-widget"]);
}
