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
/// kitty. Named pools are only *declared*: their state materialises on first use,
/// so no case provisions a lane by hand.
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

    /// Seeds `pool`'s durable Run records with one Run costing `micro_usd`, so a
    /// declared cap has already run out without launching anything.
    fn seed_spend(&self, pool: &str, worker: &str, micro_usd: u64) {
        let directory = self.path().join(".jj/pools").join(pool);
        fs::create_dir_all(&directory).expect("pool directory");
        fs::write(
            directory.join("runs.jsonl"),
            format!(
                "{}\n",
                serde_json::json!({
                    "run_id": format!("{worker}@1"),
                    "worker": worker,
                    "cost_micro_usd": micro_usd,
                    "conclusion": "succeeded",
                    "completed_at": "2026-10-15T09:00:00Z",
                })
            ),
        )
        .expect("run records");
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
/// routes `features`. Each is one Worker, provisioned when its first Dispatch
/// materialises it.
fn declare_two_lanes(fixture: &RoutingFixture) {
    fixture.write_repo_config(
        "[pools.bugs]\nsize = 1\nlabel = \"bug\"\n\n\
         [pools.features]\nsize = 1\nlabel = \"features\"\n",
    );
}

/// The one Worker a materialised single-Worker lane holds, read from its state:
/// the Dispatch provisions it, so the test cannot name it in advance.
fn lane_worker(fixture: &RoutingFixture, pool: &str) -> String {
    let text = fs::read_to_string(
        fixture
            .path()
            .join(".jj/pools")
            .join(pool)
            .join("pool.json"),
    )
    .expect("the lane's materialised state");
    let value: serde_json::Value = serde_json::from_str(&text).expect("pool state is JSON");
    value["workers"][0].as_str().expect("a Worker").to_owned()
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
    let bugs_worker = lane_worker(&fixture, "bugs");
    let features_worker = lane_worker(&fixture, "features");
    assert_eq!(
        entry(&json, "widgets--01-bug")["worker"],
        bugs_worker.as_str()
    );
    assert_eq!(
        entry(&json, "widgets--02-feature")["worker"],
        features_worker.as_str()
    );
    // Each lane's own state records the Ticket it took, so the routing is not
    // only what the batch said.
    let bugs = fs::read_to_string(
        fixture
            .path()
            .join(format!(".jj/pools/bugs/{bugs_worker}.json")),
    )
    .expect("bugs Worker state");
    assert!(bugs.contains("\"ticket\": \"widgets--01-bug\""), "{bugs}");
    let features = fs::read_to_string(
        fixture
            .path()
            .join(format!(".jj/pools/features/{features_worker}.json")),
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
    assert_eq!(
        entry(&json, "widgets--01-bug")["worker"],
        lane_worker(&fixture, "bugs").as_str()
    );
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

/// The ready status is per pool, exactly as the routing label is: a review lane
/// that declares `ready_status = "ready-for-review"` draws work the default lane
/// does not, from the same `.scratch/` tree, so a lane can define readiness
/// differently rather than every lane drawing the one global status.
#[test]
fn a_pools_ready_status_filters_its_own_discovery() {
    let fixture = RoutingFixture::new();
    fixture.write_repo_config(
        "[pools.bugs]\nsize = 1\nlabel = \"bug\"\nready_status = \"ready-for-agent\"\n\n\
         [pools.review]\nsize = 1\nlabel = \"review\"\nready_status = \"ready-for-review\"\n",
    );
    fixture.write_ticket(
        "01-agent.md",
        "# Ready for an agent\n\nStatus: ready-for-agent\nLabels: bug\n\n## Problem\n\nDo it.\n",
    );
    fixture.write_ticket(
        "02-review.md",
        "# Ready for review\n\nStatus: ready-for-review\nLabels: review\n\n## Problem\n\nDo it.\n",
    );

    let output = fixture.jjfx(&["dispatch", "--all", "--json"]);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    // Both Tickets are drawn - each by the lane whose ready status it is in, not
    // by a single global status the review Ticket would fail.
    let json: serde_json::Value =
        serde_json::from_str(stdout(&output).trim()).expect("a JSON batch");
    assert_eq!(json.as_array().expect("a batch").len(), 2, "{json}");
    assert_eq!(
        entry(&json, "widgets--01-agent")["worker"],
        lane_worker(&fixture, "bugs").as_str()
    );
    assert_eq!(
        entry(&json, "widgets--02-review")["worker"],
        lane_worker(&fixture, "review").as_str()
    );
}

/// An explicit `--pool` uses that pool's ready status too, not the global one, so
/// a lane that defines readiness differently is reachable by name as well as by
/// the `--all` fan-out.
#[test]
fn an_explicit_pool_uses_its_own_ready_status() {
    let fixture = RoutingFixture::new();
    fixture.write_repo_config(
        "[pools.bugs]\nsize = 1\nlabel = \"bug\"\n\n\
         [pools.review]\nsize = 1\nlabel = \"review\"\nready_status = \"ready-for-review\"\n",
    );
    fixture.write_ticket(
        "01-review.md",
        "# Ready for review\n\nStatus: ready-for-review\nLabels: review\n\n## Problem\n\nDo it.\n",
    );

    let output = fixture.jjfx(&["dispatch", "--all", "--pool", "review", "--json"]);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let json: serde_json::Value =
        serde_json::from_str(stdout(&output).trim()).expect("a JSON batch");
    assert_eq!(json.as_array().expect("a batch").len(), 1, "{json}");
    assert_eq!(
        entry(&json, "widgets--01-review")["worker"],
        lane_worker(&fixture, "review").as_str()
    );
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
        lane_worker(&fixture, "features").as_str()
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

/// A lane whose spend cap has run out holds its Tickets back while the other
/// lanes still dispatch: the fan-out reports one lane held rather than aborting
/// the whole batch, and the held Ticket is visible in that lane's `waiting` set.
#[test]
fn a_capped_lane_is_held_while_other_lanes_dispatch() {
    let fixture = RoutingFixture::new();
    declare_two_lanes(&fixture);
    // `bugs` declares a cap it has already run out of; `features` has room.
    fixture.write_repo_config(
        "[pools.bugs]\nlabel = \"bug\"\ncap_usd = 5.0\n\n\
         [pools.features]\nsize = 1\nlabel = \"features\"\n",
    );
    fixture.seed_spend("bugs", "worker-bugs", 6_000_000);
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
    let json: serde_json::Value =
        serde_json::from_str(stdout(&output).trim()).expect("a JSON batch");
    let held = entry(&json, "widgets--01-bug");
    assert_eq!(held["outcome"], "waiting", "{json}");
    assert!(
        held["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("pool `bugs`")),
        "the held lane is named: {json}"
    );
    // The other lane is untouched by its neighbour's cap.
    assert_eq!(
        entry(&json, "widgets--02-feature")["worker"],
        lane_worker(&fixture, "features").as_str()
    );
    let waiting = fs::read_to_string(fixture.path().join(".jj/pools/bugs/waiting.json"))
        .expect("bugs waiting set");
    assert!(waiting.contains("widgets--01-bug"), "{waiting}");
}

/// Label routing is opt-in per Repository: a Repository whose pools declare no
/// labels keeps ticket 06's refusal, because there is nothing to route by.
#[test]
fn pools_that_declare_no_label_keep_the_refusal() {
    let fixture = RoutingFixture::new();
    fixture.write_repo_config("[pools.bugs]\n\n[pools.features]\n");
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

/// A fan-out that finds nothing names every label it searched and the root it
/// searched under, so the empty batch explains itself instead of printing a
/// bare `[]`.
#[test]
fn a_fan_out_that_finds_nothing_names_every_searched_label() {
    let fixture = RoutingFixture::new();
    declare_two_lanes(&fixture);
    // No Tickets at all: every lane's search comes up empty.
    let output = fixture.jjfx(&["dispatch", "--all", "--json"]);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let json: serde_json::Value =
        serde_json::from_str(stdout(&output).trim()).expect("a JSON batch");
    let entries = json.as_array().expect("a batch");
    let searches = entries
        .iter()
        .filter(|entry| entry["ticket"].is_null())
        .collect::<Vec<_>>();
    let labels = searches
        .iter()
        .filter_map(|entry| entry["label"].as_str())
        .collect::<Vec<_>>();
    assert!(labels.contains(&"bug"), "{json}");
    assert!(labels.contains(&"features"), "{json}");
    // Every search names the same root it looked under.
    for search in searches {
        assert_eq!(search["outcome"], "skipped");
        assert!(
            search["root"]
                .as_str()
                .is_some_and(|root| root.ends_with(".scratch")),
            "{json}"
        );
    }
}

/// A pool-scoped `--all` that finds nothing names that lane's own label, exactly
/// as the single-pool path names the global one.
#[test]
fn a_pool_scoped_empty_ready_set_carries_that_lanes_label() {
    let fixture = RoutingFixture::new();
    declare_two_lanes(&fixture);
    // A Ticket no lane routes: found by discovery, routed nowhere.
    fixture.write_ticket(
        "01-ui.md",
        "# Polish the panel\n\nStatus: ready-for-agent\nLabels: ui\n\n## Problem\n\nDo it.\n",
    );

    let output = fixture.jjfx(&["dispatch", "--all", "--pool", "bugs", "--json"]);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output)
    );
    let json: serde_json::Value =
        serde_json::from_str(stdout(&output).trim()).expect("a JSON batch");
    let entries = json.as_array().expect("a batch");
    let search = entries
        .iter()
        .find(|entry| entry["ticket"].is_null())
        .expect("the batch names the search that found nothing");
    // The lane's routing label, not the global default.
    assert_eq!(search["label"], "bug", "{json}");
    assert!(
        search["root"]
            .as_str()
            .is_some_and(|root| root.ends_with(".scratch")),
        "{json}"
    );
    // The discovered-but-unrouted Ticket is still named.
    assert!(
        entries
            .iter()
            .any(|entry| entry["ticket"] == "widgets--01-ui"),
        "{json}"
    );
}
