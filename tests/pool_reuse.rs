//! A finished Worker returns to the pool, so a Pool of N serves more than N.
//!
//! A Worker whose Run has finished reads `done`, not `idle`, and nothing used to
//! return it to the pool: a later Dispatch saw no capacity even though the Worker
//! sat there with its Workspace intact, so a pool of N served N Tickets, ever,
//! until an operator ran `jjfx pool reset` between rounds. These cases drive the
//! real binary through successive rounds of a backlog larger than the pool and
//! assert every Ticket runs without a manual `pool reset`.
#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

mod support;

/// A jj Repository with a sized Worker Pool, a user config of its own, and a
/// stub terminal plus a fake `claude` on `PATH`, so a Dispatch launches a Run
/// without a real Agent Runtime.
struct ReuseFixture {
    root: TempDir,
    config_home: TempDir,
    /// The fixture's own jj config, passed to every child as `JJ_CONFIG`.
    jj_config: PathBuf,
    /// Holds the stub `kitten` and the fake `claude`; only its lifetime matters.
    _bin: TempDir,
    env_path: OsString,
}

impl ReuseFixture {
    fn new(pool_size: usize, tickets: usize) -> Self {
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
        // A jj user identity, because a Dispatch resolves its delivery identity
        // from `jj user.email`/`user.name` - blank on a clean-config runner. The
        // fixture owns it through `JJ_CONFIG` rather than the developer's
        // `~/.config/jj`, which an inherited `JJ_CONFIG` would otherwise shadow.
        let jj_config = support::hermetic_jj_config_file(config_home.path());
        let bin = TempDir::new().expect("temporary stub bin");
        let env_path = support::stub_terminal(bin.path());
        let fixture = Self {
            root,
            config_home,
            jj_config,
            _bin: bin,
            env_path,
        };
        fixture
            .write_user_config("[tickets]\nsource = \"local\"\n\n[checks]\ncommand = \"true\"\n");
        for index in 1..=tickets {
            fixture.write_ticket(index);
        }
        fixture.jj(&[
            "git",
            "remote",
            "add",
            "origin",
            "git@github.com:owner/name.git",
        ]);
        fixture.with_fake_agent();
        fixture.with_pool(pool_size);
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
            .env("JJ_CONFIG", &self.jj_config)
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .output()
            .expect("jj should be installed")
    }

    fn write_user_config(&self, content: &str) {
        let directory = self.config_home.path().join("jjfx");
        fs::create_dir_all(&directory).expect("config directory");
        fs::write(directory.join("config.toml"), content).expect("config file");
    }

    /// Writes a ready local Ticket, so `dispatch --all` has more work than a
    /// pool smaller than it can hold at once.
    fn write_ticket(&self, index: usize) {
        let directory = self.path().join(".scratch/widgets/issues");
        fs::create_dir_all(&directory).expect("issues directory");
        fs::write(
            directory.join(format!("{index:02}-ticket.md")),
            format!(
                "# Ticket {index}\n\nStatus: ready-for-agent\nType: task\nBlocked by: none\n\n## Problem\n\nDo it.\n"
            ),
        )
        .expect("Ticket file");
    }

    /// Puts a fake `claude` on `PATH` that exits successfully, so a Dispatch can
    /// launch a Run that finishes on its own.
    fn with_fake_agent(&self) {
        let agent = self._bin.path().join("claude");
        fs::write(
            &agent,
            "#!/bin/sh\nif [ \"$1\" = \"--help\" ]; then exit 0; fi\nprintf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\"}'\n",
        )
        .expect("fake agent");
        fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).expect("executable agent");
    }

    /// Sizes the Worker Pool, provisioning its Workers and their Workspaces.
    fn with_pool(&self, size: usize) {
        let output = self
            .jjfx(&["pool", &size.to_string()])
            .output()
            .expect("pool");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn jjfx(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_jjfx"));
        command
            .args(args)
            .current_dir(self.path())
            .env("JJ_CONFIG", &self.jj_config)
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .env("PATH", &self.env_path);
        command
    }

    /// Runs `dispatch`, retrying a Dispatch that failed closed because the stub
    /// terminal was starved past its deadline under load - see
    /// [`support::run_jjfx_tolerating_a_starved_terminal`].
    fn dispatch(&self, args: &[&str]) -> Output {
        support::run_jjfx_tolerating_a_starved_terminal(|| self.jjfx(args))
    }

    fn run(&self, args: &[&str]) -> Output {
        self.jjfx(args).output().expect("jjfx should run")
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The `(ticket, worker)` of every `dispatched` entry in a Dispatch batch, in
/// input order.
fn dispatched(entries: &Value) -> Vec<(String, String)> {
    entries
        .as_array()
        .expect("an array of outcomes")
        .iter()
        .filter(|entry| entry["outcome"] == "dispatched")
        .map(|entry| {
            (
                entry["ticket"].as_str().expect("a ticket").to_owned(),
                entry["worker"].as_str().expect("a worker").to_owned(),
            )
        })
        .collect()
}

/// The tickets of every `waiting` entry in a Dispatch batch, in order.
fn waiting(entries: &Value) -> Vec<String> {
    entries
        .as_array()
        .expect("an array of outcomes")
        .iter()
        .filter(|entry| entry["outcome"] == "waiting")
        .map(|entry| entry["ticket"].as_str().expect("a ticket").to_owned())
        .collect()
}

fn batch(output: &Output) -> Value {
    serde_json::from_str(stdout(output).trim()).expect("a JSON batch")
}

/// The criterion the ticket exists for: a backlog larger than the pool runs to
/// completion across successive rounds, with no manual `jjfx pool reset`.
///
/// Round one fills the two-Worker pool and leaves two Tickets waiting. The Runs
/// finish, so both Workers read `done`. Round two must reuse them: the finished
/// Workers return to the pool automatically, so the remaining two Tickets run
/// too. Every Ticket runs, and this test never invokes `jjfx pool reset`.
#[test]
fn a_pool_smaller_than_the_backlog_serves_every_ticket_without_a_manual_reset() {
    let fixture = ReuseFixture::new(2, 4);

    let first = fixture.dispatch(&["dispatch", "--all", "--json"]);
    assert!(
        first.status.success(),
        "{}\n{}",
        stdout(&first),
        stderr(&first)
    );
    let first_json = batch(&first);
    let first_dispatched = dispatched(&first_json);
    assert_eq!(
        first_dispatched.len(),
        2,
        "the two-Worker pool fills: {}",
        stdout(&first)
    );
    assert_eq!(
        waiting(&first_json).len(),
        2,
        "the rest of the backlog waits: {}",
        stdout(&first)
    );

    // The Runs finish on their own (the fake agent exits), so both Workers reach
    // the terminal state a later Dispatch must be able to reuse.
    let busy = first_dispatched
        .iter()
        .map(|(_, worker)| worker.as_str())
        .collect::<Vec<_>>();
    let mut wait = vec!["wait"];
    wait.extend(busy.iter().copied());
    wait.extend(["--until", "terminal", "--timeout", "120"]);
    let waited = fixture.run(&wait);
    assert!(
        waited.status.success(),
        "the first round's Runs must finish: {}\n{}",
        stdout(&waited),
        stderr(&waited)
    );

    // No `jjfx pool reset` here: the finished Workers are the capacity round two
    // runs on.
    let second = fixture.dispatch(&["dispatch", "--all", "--json"]);
    assert!(
        second.status.success(),
        "{}\n{}",
        stdout(&second),
        stderr(&second)
    );
    let second_json = batch(&second);
    let second_dispatched = dispatched(&second_json);
    assert_eq!(
        second_dispatched.len(),
        2,
        "the finished Workers must be reused: {}",
        stdout(&second)
    );

    let mut ran = first_dispatched
        .iter()
        .chain(&second_dispatched)
        .map(|(ticket, _)| ticket.clone())
        .collect::<Vec<_>>();
    ran.sort();
    assert_eq!(
        ran,
        [
            "widgets--01-ticket",
            "widgets--02-ticket",
            "widgets--03-ticket",
            "widgets--04-ticket"
        ],
        "every Ticket in the backlog must run"
    );
}
