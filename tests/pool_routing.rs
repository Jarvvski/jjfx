//! Routing a Ticket to its Worker Pool by label (ticket 07).
//!
//! Requiring `--pool` on every Dispatch makes lanes capacity buckets a human has
//! to route by hand. A pool declares the label that routes to it, and
//! `dispatch --all` fans out across the pools that declare one, sending each
//! Ticket to the pool whose label it carries. These tests drive the real binary
//! against a real jj repository to prove the decisions: each Ticket lands in its
//! own lane, a Ticket that matches no pool is **reported, not dropped**, an
//! explicit `--pool` still means "only this lane", and ticket 06's loud refusal
//! for an unqualified named Dispatch is untouched.
#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

mod support;

/// A throwaway repository with an origin, a fake Agent Runtime and a stub
/// terminal, so a Dispatch can launch a Run without a real agent or a real
/// kitty. Named pools are provisioned per case.
///
/// Worker Workspaces are pinned under `<root>/ws` with `JJ_WS_DIR` so the fixture
/// owns every path it creates (a sibling `<repo>-workspaces` directory would sit
/// beside the `TempDir` and outlive it).
struct RoutingFixture {
    root: TempDir,
    config_home: TempDir,
    bin: TempDir,
    /// `PATH` with the fixture's `bin` ahead of the inherited one, so the stub
    /// terminal and the fake Agent are what the binary under test finds.
    env_path: OsString,
}

impl RoutingFixture {
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
        // A base commit carrying `main`, so trunk resolves and a Worker
        // Workspace can be added against it.
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

    fn jj_config_path(&self) -> PathBuf {
        self.config_home.path().join("jj.toml")
    }

    fn jj_in(&self, directory: &Path, args: &[&str]) -> Output {
        Command::new("jj")
            .args(["--config", "signing.behavior=drop"])
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

    fn write_config(&self, content: &str) {
        let directory = self.config_home.path().join("jjfx");
        fs::create_dir_all(&directory).expect("config directory");
        fs::write(directory.join("config.toml"), content).expect("config file");
    }

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

    /// Provisions one named lane: its state, one idle Worker, and that Worker's
    /// jj Workspace, so a Dispatch reaches a launched Run rather than failing on
    /// a missing pool or Workspace.
    fn with_lane(&self, pool: &str, worker: &str) {
        let directory = self.path().join(".jj").join("pools").join(pool);
        fs::create_dir_all(&directory).expect("pool directory");
        fs::write(
            directory.join("pool.json"),
            format!(
                "{{\"created_at\":\"2026-07-27T10:00:00Z\",\"gh_repo\":\"\",\"size\":1,\
                 \"workers\":[\"{worker}\"]}}"
            ),
        )
        .expect("pool state");
        fs::write(
            directory.join(format!("{worker}.json")),
            "{\"agent\":\"claude\",\"branch_name\":null,\"completed_at\":null,\"error\":null,\
             \"exit_code\":null,\"log_file\":null,\"pid\":null,\"started_at\":null,\
             \"status\":\"idle\",\"ticket\":null}",
        )
        .expect("worker state");
        fs::create_dir_all(self.ws_dir()).expect("workspace base");
        self.jj_ok(&[
            "workspace",
            "add",
            "--name",
            worker,
            "-r",
            "main",
            self.ws_dir()
                .join(worker)
                .to_str()
                .expect("workspace path is UTF-8"),
        ]);
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
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// One Ticket object from a `dispatch --json` batch, by Ticket id.
fn entry<'a>(json: &'a serde_json::Value, ticket: &str) -> &'a serde_json::Value {
    json.as_array()
        .expect("a batch is an array")
        .iter()
        .find(|entry| entry["ticket"] == ticket)
        .unwrap_or_else(|| panic!("no entry for {ticket}: {json}"))
}

/// The two lanes this fixture's cases share: `bugs` routes `bug`, `features`
/// routes `features`.
fn declare_two_lanes(fixture: &RoutingFixture) {
    fixture.write_repo_config(
        "[pools.bugs]\nlabel = \"bug\"\n\n[pools.features]\nlabel = \"features\"\n",
    );
    fixture.with_lane("bugs", "worker-bugs");
    fixture.with_lane("features", "worker-features");
}

/// Each Ticket goes to the pool whose label it carries, in one `--all` call.
#[test]
fn dispatch_all_routes_each_ticket_to_the_pool_whose_label_it_carries() {
    let fixture = RoutingFixture::new();
    declare_two_lanes(&fixture);
    fixture.write_ticket(
        "01-bug.md",
        "# Fix the crash\n\nStatus: ready-for-agent\nLabels: bug\n\n## Problem\n\nDo it.\n",
    );
    fixture.write_ticket(
        "02-feature.md",
        "# Ship the widget\n\nStatus: ready-for-agent\nLabels: features\n\n## Problem\n\nDo it.\n",
    );

    let output = fixture.jjfx(&["dispatch", "--all", "--json"]);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let error = stderr(&output);
    assert!(error.contains("label 'bug' for pool 'bugs'"), "{error}");
    assert!(
        error.contains("label 'features' for pool 'features'"),
        "{error}"
    );
    let json: serde_json::Value =
        serde_json::from_str(stdout(&output).trim()).expect("a JSON batch");
    assert_eq!(entry(&json, "widgets--01-bug")["worker"], "worker-bugs");
    assert_eq!(
        entry(&json, "widgets--02-feature")["worker"],
        "worker-features"
    );
    // Each lane's own state records the Ticket it took, so the routing is not
    // only what the batch said.
    let bugs = fs::read_to_string(fixture.path().join(".jj/pools/bugs/worker-bugs.json"))
        .expect("bugs Worker state");
    assert!(bugs.contains("\"ticket\": \"widgets--01-bug\""), "{bugs}");
    let features = fs::read_to_string(
        fixture
            .path()
            .join(".jj/pools/features/worker-features.json"),
    )
    .expect("features Worker state");
    assert!(
        features.contains("\"ticket\": \"widgets--02-feature\""),
        "{features}"
    );
}

/// A Ticket matching no pool's label is reported with the reason, never
/// silently skipped - the same principle as ticket 06's ambiguity refusal.
#[test]
fn a_ticket_matching_no_pool_is_reported_not_dropped() {
    let fixture = RoutingFixture::new();
    declare_two_lanes(&fixture);
    fixture.write_ticket(
        "01-bug.md",
        "# Fix the crash\n\nStatus: ready-for-agent\nLabels: bug\n\n## Problem\n\nDo it.\n",
    );
    fixture.write_ticket(
        "02-ui.md",
        "# Polish the panel\n\nStatus: ready-for-agent\nLabels: ui\n\n## Problem\n\nDo it.\n",
    );

    let output = fixture.jjfx(&["dispatch", "--all", "--json"]);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let error = stderr(&output);
    assert!(
        error.contains("widgets--02-ui: missing label"),
        "an unroutable Ticket must be reported with its reason:\n{error}"
    );
    assert!(error.contains("\"ui\""), "{error}");
    // It was reported, not dispatched: only the routed Ticket is in the batch.
    let json: serde_json::Value =
        serde_json::from_str(stdout(&output).trim()).expect("a JSON batch");
    assert_eq!(json.as_array().expect("a batch").len(), 1, "{json}");
    assert_eq!(entry(&json, "widgets--01-bug")["worker"], "worker-bugs");
}

/// A Ticket that declares no `Labels:` carries the ready label alone, never a
/// pool's routing label - so it cannot match every lane, and a Repository with
/// several lanes reports it instead of guessing.
#[test]
fn an_untagged_ticket_does_not_route_to_a_named_lane() {
    let fixture = RoutingFixture::new();
    declare_two_lanes(&fixture);
    fixture.write_ticket(
        "01-untagged.md",
        "# Untagged\n\nStatus: ready-for-agent\n\n## Problem\n\nDo it.\n",
    );

    let output = fixture.jjfx(&["dispatch", "--all", "--json"]);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    // Nothing routed, so the batch is the reported set: the untagged Ticket is
    // named, with the labels it carried, rather than collapsing to `[]`.
    let json: serde_json::Value =
        serde_json::from_str(stdout(&output).trim()).expect("a JSON batch");
    let reported = entry(&json, "widgets--01-untagged");
    assert_eq!(reported["outcome"], "skipped");
    assert!(
        reported["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("ready-for-agent")),
        "{json}"
    );

    // The prose path says the same thing on stderr.
    let prose = fixture.jjfx(&["dispatch", "--all"]);
    assert!(prose.status.success(), "{}", stderr(&prose));
    let error = stderr(&prose);
    assert!(error.contains("No routable tickets:"), "{error}");
    assert!(error.contains("widgets--01-untagged"), "{error}");
    assert!(error.contains("ready-for-agent"), "{error}");
}

/// An explicit `--pool` still wins, and still means "only this lane".
#[test]
fn an_explicit_pool_overrides_label_routing() {
    let fixture = RoutingFixture::new();
    declare_two_lanes(&fixture);
    fixture.write_ticket(
        "01-bug.md",
        "# Fix the crash\n\nStatus: ready-for-agent\nLabels: bug\n\n## Problem\n\nDo it.\n",
    );
    fixture.write_ticket(
        "02-feature.md",
        "# Ship the widget\n\nStatus: ready-for-agent\nLabels: features\n\n## Problem\n\nDo it.\n",
    );

    let output = fixture.jjfx(&["dispatch", "--all", "--pool", "features", "--json"]);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let error = stderr(&output);
    assert!(
        !error.contains("for pool 'bugs'"),
        "an explicit --pool must not fan out:\n{error}"
    );
    let json: serde_json::Value =
        serde_json::from_str(stdout(&output).trim()).expect("a JSON batch");
    assert_eq!(json.as_array().expect("a batch").len(), 1, "{json}");
    assert_eq!(
        entry(&json, "widgets--02-feature")["worker"],
        "worker-features"
    );
}

/// Ticket 06's primitive is untouched: label routing is the convenience for
/// `--all`, so an unqualified Dispatch that names a Ticket is still refused
/// loudly, naming the pools that exist.
#[test]
fn an_unqualified_named_dispatch_is_still_refused_when_pools_declare_labels() {
    let fixture = RoutingFixture::new();
    declare_two_lanes(&fixture);
    fixture.write_ticket(
        "01-bug.md",
        "# Fix the crash\n\nStatus: ready-for-agent\nLabels: bug\n\n## Problem\n\nDo it.\n",
    );

    let output = fixture.jjfx(&["dispatch", "widgets--01-bug"]);

    assert!(
        !output.status.success(),
        "an unqualified named Dispatch into several pools must refuse:\nstdout:\n{}",
        stdout(&output)
    );
    let error = stderr(&output);
    assert!(error.contains("default"), "{error}");
    assert!(error.contains("bugs"), "{error}");
    assert!(error.contains("features"), "{error}");
    assert!(error.contains("--pool"), "{error}");
}

/// Label routing is opt-in per Repository: a Repository whose pools declare no
/// labels keeps ticket 06's refusal, because there is nothing to route by.
#[test]
fn pools_that_declare_no_label_keep_the_refusal() {
    let fixture = RoutingFixture::new();
    fixture.write_repo_config("[pools.bugs]\n\n[pools.features]\n");
    fixture.with_lane("bugs", "worker-bugs");
    fixture.with_lane("features", "worker-features");
    fixture.write_ticket(
        "01-bug.md",
        "# Fix the crash\n\nStatus: ready-for-agent\nLabels: bug\n\n## Problem\n\nDo it.\n",
    );

    let output = fixture.jjfx(&["dispatch", "--all"]);

    assert!(
        !output.status.success(),
        "a Repository with no routing labels must keep refusing:\nstdout:\n{}",
        stdout(&output)
    );
    let error = stderr(&output);
    assert!(error.contains("bugs"), "{error}");
    assert!(error.contains("features"), "{error}");
    assert!(error.contains("--pool"), "{error}");
}
