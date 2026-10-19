//! `status --json` reports every Worker Pool a Repository holds.
//!
//! The shape is deliberately additive: `workers[]` and `waiting[]` stay flat and
//! each entry names its `pool`, so `jq '.workers[] | select(.pool == "bugs")'`
//! answers the per-pool question, and per-pool spend arrives as a sibling `pools`
//! key carrying a subtotal under the Repository-wide total. A pool directory no
//! declaration claims is named rather than adopted.
//!
//! Each case drives the real binary, because the point is the document an agent
//! reads: the JSON a `jq` filter sees, and the prose a human reads.
#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

/// A real jj Repository with a user config home of its own, so the binary under
/// test never reads the developer's own config.
struct Fixture {
    root: TempDir,
    config_home: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().expect("temporary repository");
        let config_home = TempDir::new().expect("temporary config home");
        let fixture = Self { root, config_home };
        fixture.jj(&["git", "init", "--no-colocate"]);
        fixture.write_user_config("");
        // The delivery identity is passed as `JJ_CONFIG` rather than set with
        // `jj config set --repo`: jj stores repo config under a directory
        // derived from `XDG_CONFIG_HOME`, which this fixture points at its own
        // config home, so a `--repo` write would land where the binary never
        // looks.
        fs::write(
            fixture.config_home.path().join("jj.toml"),
            "user.name = \"Test User\"\nuser.email = \"test@example.com\"\n",
        )
        .expect("jj config file");
        fixture
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn jj(&self, args: &[&str]) -> Output {
        Command::new("jj")
            .args(["--config", "signing.behavior=drop"])
            .args(args)
            .current_dir(self.path())
            .output()
            .expect("jj should be installed")
    }

    /// The user-global jjfx config, so the binary reads a known home.
    fn write_user_config(&self, content: &str) {
        let directory = self.config_home.path().join("jjfx");
        fs::create_dir_all(&directory).expect("config directory");
        fs::write(directory.join("config.toml"), content).expect("config file");
    }

    /// The Repository's own `.jjfx/config.toml`, which declares the pools.
    fn write_repo_config(&self, content: &str) {
        let directory = self.path().join(".jjfx");
        fs::create_dir_all(&directory).expect("repository config directory");
        fs::write(directory.join("config.toml"), content).expect("repository config file");
    }

    /// Seeds `.jj/pools/<pool>/` with a one-Worker idle pool, the state a pool
    /// materialises on first use.
    fn seed_pool(&self, pool: &str, worker: &str) {
        let directory = self.path().join(".jj/pools").join(pool);
        fs::create_dir_all(&directory).expect("pool directory");
        fs::write(
            directory.join("pool.json"),
            format!(
                r#"{{"size":1,"gh_repo":"Jarvvski/jjfx","workers":["{worker}"],"created_at":"2026-07-27T10:00:00Z","names":{{}}}}"#
            ),
        )
        .expect("pool state");
        fs::write(
            directory.join(format!("{worker}.json")),
            br#"{"status":"idle","agent":"claude","ticket":null,"pid":null,"started_at":null,"completed_at":null,"log_file":null,"branch_name":null,"exit_code":null,"error":null}"#,
        )
        .expect("worker state");
    }

    /// Seeds `.jj/pools/<pool>/pool.json` with bytes that cannot be decoded -
    /// the unreadable case, which is a real error rather than the normal absence
    /// of a lane that has never run.
    fn seed_malformed_pool(&self, pool: &str) {
        let directory = self.path().join(".jj/pools").join(pool);
        fs::create_dir_all(&directory).expect("pool directory");
        fs::write(directory.join("pool.json"), b"{ not json").expect("malformed pool state");
    }

    fn jjfx(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_jjfx"))
            .args(args)
            .current_dir(self.path())
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .env("JJ_CONFIG", self.config_home.path().join("jj.toml"))
            .output()
            .expect("jjfx should run")
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The default pool, a declared `bugs` pool with a cap, and a stray directory no
/// declaration claims.
fn fixture() -> Fixture {
    let fixture = Fixture::new();
    fixture.write_repo_config("[pools.bugs]\nsize = 1\ncap_usd = 5.0\n");
    fixture.seed_pool("default", "worker-01");
    fixture.seed_pool("bugs", "worker-02");
    fixture.seed_pool("stray", "worker-03");
    fixture
}

#[test]
fn status_json_reports_every_pool_and_its_workers() {
    let fixture = fixture();
    let output = fixture.jjfx(&["status", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");

    // The documented filter: `jq '.workers[] | select(.pool == "bugs")'`.
    let bugs = value["workers"]
        .as_array()
        .expect("workers array")
        .iter()
        .filter(|worker| worker["pool"] == "bugs")
        .collect::<Vec<_>>();
    assert_eq!(bugs.len(), 1, "{}", stdout(&output));
    assert_eq!(bugs[0]["worker"], "worker-02");
    // The stray pool's Worker is not reported as one of the Repository's.
    assert!(
        value["workers"]
            .as_array()
            .expect("workers")
            .iter()
            .all(|worker| worker["pool"] == "default" || worker["pool"] == "bugs"),
        "{}",
        stdout(&output)
    );

    // Per-pool subtotals: `default` implicit with no cap, `bugs` declared with one.
    let pools = value["pools"].as_array().expect("pools array");
    assert_eq!(pools[0]["name"], "default");
    assert_eq!(pools[0]["implicit"], true);
    assert_eq!(pools[0]["cap"], Value::Null);
    assert_eq!(pools[1]["name"], "bugs");
    assert_eq!(pools[1]["implicit"], false);
    assert_eq!(pools[1]["size"], 1);
    assert_eq!(pools[1]["cap"], 5.0);
    assert_eq!(pools[1]["window"], "week");
    assert!(pools[1]["spend"].is_number(), "{}", stdout(&output));

    // A stray directory is named, never adopted.
    assert_eq!(value["orphans"], serde_json::json!(["stray"]));
}

#[test]
fn listing_declared_but_unmaterialised_pools_is_silent_on_stderr() {
    let fixture = Fixture::new();
    // Two declared lanes with no `.jj/pools/<name>/` directory, plus the
    // implicit `default`, which is unmaterialised too. Absence is the normal
    // case - a declared lane materialises on first use (ADR 0013) - so listing
    // must not narrate it once per lane.
    fixture.write_repo_config("[pools.alpha]\nsize = 1\n[pools.beta]\nsize = 2\n");
    let output = fixture.jjfx(&["pool", "list"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).is_empty(),
        "a successful list must not narrate absence: {:?}",
        stderr(&output)
    );
    // Dropping the fragment lost nothing: the table still names every lane.
    let text = stdout(&output);
    assert!(text.contains("alpha"), "{text}");
    assert!(text.contains("beta"), "{text}");
}

#[test]
fn a_malformed_pool_document_is_reported_naming_its_lane() {
    let fixture = Fixture::new();
    // `alpha` holds a document that exists but cannot be decoded - a real error,
    // so it is reported - while `beta` has no state at all, the normal absent
    // case, which must stay silent.
    fixture.write_repo_config("[pools.alpha]\nsize = 1\n[pools.beta]\nsize = 1\n");
    fixture.seed_malformed_pool("alpha");

    // `pool list` and prose `status` are the same command, but the point is the
    // text a human reads on either spelling, so drive both.
    for args in [&["pool", "list"][..], &["status"][..]] {
        let output = fixture.jjfx(args);
        assert!(output.status.success(), "{}", stderr(&output));
        let text = stderr(&output);
        assert!(
            text.contains("alpha"),
            "`{}` must name the corrupt lane: {text:?}",
            args[0]
        );
        assert!(
            text.contains("cannot decode Worker Pool"),
            "`{}` must keep the malformed detail: {text:?}",
            args[0]
        );
        assert!(
            !text.contains("beta"),
            "`{}` must stay silent about an absent lane: {text:?}",
            args[0]
        );
    }
}

#[test]
fn status_prose_names_the_pool_its_spend_and_any_orphan() {
    let fixture = fixture();
    let output = fixture.jjfx(&["status"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);

    // The Worker table names each Worker's pool, and the Pools table shows the
    // pool's spend against its cap.
    assert!(text.contains("POOL"), "{text}");
    assert!(text.contains("SPEND"), "{text}");
    assert!(text.contains("bugs"), "{text}");
    assert!(text.contains("$5.0000"), "the bugs cap is shown: {text}");
    assert!(text.contains("stray"), "the orphan is named: {text}");
}
