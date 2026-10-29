//! `jjfx ticket list`: the read-only Ticket backlog and its readiness.
//!
//! The command must list every Ticket the configured source discovers, with its
//! `Status`, its Blockers and whether it is ready, must report the same verdict
//! `dispatch --all` prints its blocked line from, and must change nothing.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

mod support;

/// A `jj` repository plus a `[tickets] source = "local"` config home.
fn jj_repository() -> (TempDir, TempDir) {
    let repository = TempDir::new().expect("temporary repository");
    let status = Command::new("jj")
        .args([
            "--config",
            "signing.behavior=drop",
            "git",
            "init",
            "--no-colocate",
        ])
        .arg(repository.path())
        .status()
        .expect("jj should be installed");
    assert!(status.success(), "jj git init");
    let config_home = TempDir::new().expect("temporary config home");
    let config_directory = config_home.path().join("jjfx");
    fs::create_dir_all(&config_directory).expect("config directory");
    fs::write(
        config_directory.join("config.toml"),
        "[tickets]\nsource = \"local\"\n",
    )
    .expect("config file");
    (repository, config_home)
}

fn write_ticket(root: &Path, feature: &str, file: &str, content: &str) {
    let directory = root.join(".scratch").join(feature).join("issues");
    fs::create_dir_all(&directory).expect("issues directory");
    fs::write(directory.join(file), content).expect("Ticket file");
}

/// Append one succeeded Run Record carrying a passing check, which is the
/// durable signal a reconcile would resolve its Ticket from.
fn write_run_record(root: &Path, ticket: &str) {
    let pool = root.join(".jj").join("pools").join("default");
    fs::create_dir_all(&pool).expect("pool directory");
    let line = format!(
        "{{\"run_id\":\"worker-1@2026-01-01T00:00:00Z\",\"worker\":\"worker-1\",\
         \"ticket\":\"{ticket}\",\"conclusion\":\"succeeded\",\"check\":\"passed\"}}\n"
    );
    fs::write(pool.join("runs.jsonl"), line).expect("run record");
}

fn ticket_file(root: &Path, feature: &str, file: &str) -> String {
    fs::read_to_string(
        root.join(".scratch")
            .join(feature)
            .join("issues")
            .join(file),
    )
    .expect("Ticket file")
}

/// Runs `jjfx ticket list` with a stub terminal on `PATH`, so the test drives the
/// real seam the Dispatch commands use rather than depending on kitty.
///
/// An inherited `JJ_CONFIG` would take precedence over `XDG_CONFIG_HOME`, so it
/// is dropped: the fixture owns its jj config.
fn run_ticket_list(repository: &Path, config_home: &Path, extra: &[&str]) -> std::process::Output {
    let bin = TempDir::new().expect("temporary stub bin");
    let path = support::stub_terminal(bin.path());
    let mut args = vec!["ticket", "list"];
    args.extend_from_slice(extra);
    let mut command = Command::new(env!("CARGO_BIN_EXE_jjfx"));
    command
        .args(&args)
        .current_dir(repository)
        .env_remove("JJ_CONFIG")
        .env("XDG_CONFIG_HOME", config_home)
        .env("PATH", &path);
    command.output().expect("jjfx should run")
}

/// Runs `jjfx dispatch --all` on the same stub terminal the list test uses.
fn run_dispatch_all(repository: &Path, config_home: &Path) -> std::process::Output {
    let bin = TempDir::new().expect("temporary stub bin");
    let path = support::stub_terminal(bin.path());
    support::run_jjfx_tolerating_a_starved_terminal(|| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_jjfx"));
        command
            .args(["dispatch", "--all"])
            .current_dir(repository)
            .env_remove("JJ_CONFIG")
            .env("XDG_CONFIG_HOME", config_home)
            .env("PATH", &path);
        command
    })
}

/// Every file under `root` as `(relative path, bytes)`.
///
/// A command that rewrote anything durable - a Ticket file, a Run Record, the
/// queue, a Worker document - changes this fingerprint. Comparing bytes rather
/// than modification times is what makes it catch a rewrite to the same content
/// as well as a different one.
fn fingerprint(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    collect(root, root, &mut files);
    files
}

fn collect(root: &Path, directory: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
    let mut entries = fs::read_dir(directory)
        .expect("directory")
        .map(|entry| entry.expect("entry").path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect(root, &path, files);
        } else {
            let relative = path
                .strip_prefix(root)
                .expect("path under root")
                .display()
                .to_string();
            files.insert(relative, fs::read(&path).expect("file"));
        }
    }
}

/// The fixture every test reads: a claimed Ticket that blocks a ready one, a
/// resolved one, and an independent ready one.
fn backlog_fixture(repository: &Path) {
    write_ticket(
        repository,
        "widgets",
        "01-foundation.md",
        "# Foundation\n\nStatus: claimed\n",
    );
    write_ticket(
        repository,
        "widgets",
        "02-dependent.md",
        "# Dependent\n\nStatus: ready-for-agent\nBlocked by: 01\n",
    );
    write_ticket(
        repository,
        "widgets",
        "03-done.md",
        "# Done\n\nStatus: resolved\n",
    );
    write_ticket(
        repository,
        "other",
        "01-independent.md",
        "# Independent\n\nStatus: ready-for-agent\n",
    );
}

#[test]
fn ticket_list_prose_reports_every_ticket_with_its_status_and_readiness() {
    let (repository, config_home) = jj_repository();
    backlog_fixture(repository.path());

    let output = run_ticket_list(repository.path(), config_home.path(), &[]);

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Every Ticket the source discovers is listed, across features.
    for ticket in [
        "widgets--01-foundation",
        "widgets--02-dependent",
        "widgets--03-done",
        "other--01-independent",
    ] {
        assert!(stdout.contains(ticket), "missing {ticket}: {stdout}");
    }
    // Each Ticket's own Status is reported, not only the ready one.
    for status in ["claimed", "ready-for-agent", "resolved"] {
        assert!(stdout.contains(status), "missing status {status}: {stdout}");
    }
    // Readiness is explicit, and a blocked Ticket says what stops it - the same
    // wording a Dispatch prints.
    assert!(
        stdout.contains("blocked by widgets--01-foundation (claimed)"),
        "{stdout}"
    );
    assert!(stdout.contains("done"), "{stdout}");
    // The two ways of failing readiness are distinct words (drive 4, finding
    // 08). `widgets--01-foundation` is `claimed`, so it is not this lane's work
    // at all - `not-ready`, not `blocked`, which the vocabulary already uses for
    // a Ticket that *is* this lane's and waits on an unresolved Blocker, as
    // `widgets--02-dependent` does above.
    assert!(
        stdout.contains("not-ready"),
        "a Ticket whose Status is not the lane's must not read as blocked: {stdout}"
    );
    assert!(
        stdout.contains("expected status \"ready-for-agent\", found \"claimed\""),
        "{stdout}"
    );
}

#[test]
fn ticket_list_json_reports_the_lanes_and_each_ticket_as_structure() {
    let (repository, config_home) = jj_repository();
    backlog_fixture(repository.path());

    let output = run_ticket_list(repository.path(), config_home.path(), &["--json"]);

    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("valid JSON");
    let lane = &value["lanes"][0];
    assert_eq!(lane["pool"], "default");
    assert_eq!(lane["label"], "ready-for-agent");
    assert_eq!(lane["status"], "ready-for-agent");

    let tickets = value["tickets"].as_array().expect("an array of Tickets");
    let ticket = |id: &str| {
        tickets
            .iter()
            .find(|ticket| ticket["ticket"] == id)
            .unwrap_or_else(|| panic!("no entry for {id}: {value}"))
    };

    let independent = ticket("other--01-independent");
    assert_eq!(independent["title"], "Independent");
    assert_eq!(independent["status"], "ready-for-agent");
    assert_eq!(independent["state"], "ready");
    assert_eq!(independent["ready"], true);
    assert_eq!(independent["blockers"], serde_json::json!([]));
    assert!(independent["reason"].is_null(), "{independent}");

    let dependent = ticket("widgets--02-dependent");
    assert_eq!(dependent["state"], "blocked");
    assert_eq!(dependent["ready"], false);
    assert_eq!(
        dependent["blockers"],
        serde_json::json!(["widgets--01-foundation"])
    );
    assert_eq!(
        dependent["reason"],
        "blocked by widgets--01-foundation (claimed)"
    );

    // A resolved Ticket is done, not blocked: `ready` is false and there is no
    // reason, so a caller can tell finished work from stuck work.
    let done = ticket("widgets--03-done");
    assert_eq!(done["status"], "resolved");
    assert_eq!(done["state"], "done");
    assert_eq!(done["ready"], false);
    assert!(done["reason"].is_null(), "{done}");
}

/// The list and `dispatch --all` must not be able to disagree about whether a
/// Ticket is ready: the blocked line a Dispatch prints is the line the list
/// prints, because both read one verdict.
#[test]
fn ticket_list_agrees_with_dispatch_all_about_readiness() {
    let (repository, config_home) = jj_repository();
    // Nothing is ready: 01 is in flight and 02 is blocked by it.
    write_ticket(
        repository.path(),
        "widgets",
        "01-foundation.md",
        "# Foundation\n\nStatus: claimed\n",
    );
    write_ticket(
        repository.path(),
        "widgets",
        "02-dependent.md",
        "# Dependent\n\nStatus: ready-for-agent\nBlocked by: 01\n",
    );

    let dispatch = run_dispatch_all(repository.path(), config_home.path());
    let dispatch_stderr = String::from_utf8_lossy(&dispatch.stderr);
    let blocked_line = "widgets--02-dependent blocked by widgets--01-foundation (claimed)";
    assert!(dispatch_stderr.contains(blocked_line), "{dispatch_stderr}");

    let list = run_ticket_list(repository.path(), config_home.path(), &[]);
    assert!(list.status.success(), "{list:?}");
    let list_stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_stdout.contains("blocked by widgets--01-foundation (claimed)"),
        "the list disagrees with the Dispatch: {list_stdout}"
    );
}

/// The point of the command: it reconciles nothing. The fixture holds a finished
/// Run whose Ticket a reconcile would resolve, and a live Worker claim a
/// reconcile would rewrite; the list must leave every byte of the durable state
/// exactly as it found it.
#[test]
fn ticket_list_does_not_mutate_durable_state() {
    let (repository, config_home) = jj_repository();
    write_ticket(
        repository.path(),
        "widgets",
        "01-foundation.md",
        "# Foundation\n\nStatus: claimed\n",
    );
    write_ticket(
        repository.path(),
        "widgets",
        "02-dependent.md",
        "# Dependent\n\nStatus: ready-for-agent\nBlocked by: 01\n",
    );
    // A finished Run with a passing check: `dispatch --all`, `status` and
    // `ticket reconcile` all resolve 01 from this. `ticket list` must not.
    write_run_record(repository.path(), "widgets--01-foundation");
    let before = fingerprint(repository.path());

    let output = run_ticket_list(repository.path(), config_home.path(), &["--json"]);

    assert!(output.status.success(), "{output:?}");
    let after = fingerprint(repository.path());
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>(),
        "the command created or removed durable state"
    );
    assert_eq!(before, after, "the command rewrote durable state");
    // The reconcile this command must not run would have rewritten the Ticket's
    // Status; assert the observable consequence too, so the test fails loudly if
    // the fingerprint ever stops covering it.
    assert!(
        ticket_file(repository.path(), "widgets", "01-foundation.md").contains("Status: claimed"),
        "the list reconciled the Ticket instead of only reading it"
    );
    // Prove the fixture is reconcile-sensitive, so "no change" cannot pass
    // vacuously: `ticket reconcile` - the command that does settle this state -
    // rewrites it from the same starting point.
    let reconcile = Command::new(env!("CARGO_BIN_EXE_jjfx"))
        .args(["ticket", "reconcile"])
        .current_dir(repository.path())
        .env_remove("JJ_CONFIG")
        .env("XDG_CONFIG_HOME", config_home.path())
        .output()
        .expect("jjfx should run");
    assert!(reconcile.status.success(), "{reconcile:?}");
    assert_ne!(
        fingerprint(repository.path()),
        after,
        "the fixture must be reconcile-sensitive for the no-mutation assertion to mean anything"
    );
}

/// The list needs no terminal. A Dispatch fails closed when it cannot see which
/// Worker Workspaces are open (ADR 0011); a read-only backlog has no such seam,
/// so it answers with no `kitten` and no `jj` on `PATH` at all.
#[test]
fn ticket_list_needs_neither_the_terminal_nor_jj() {
    let (repository, config_home) = jj_repository();
    write_ticket(
        repository.path(),
        "widgets",
        "01-foundation.md",
        "# Foundation\n\nStatus: ready-for-agent\n",
    );

    let output = Command::new(env!("CARGO_BIN_EXE_jjfx"))
        .args(["ticket", "list", "--json"])
        .current_dir(repository.path())
        .env_remove("JJ_CONFIG")
        .env("XDG_CONFIG_HOME", config_home.path())
        // A PATH with neither `kitten` nor `jj` on it: if the command consulted
        // the terminal seam it would fail closed, and if it shelled out to jj it
        // would error.
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("jjfx should run");

    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("valid JSON");
    assert_eq!(value["tickets"][0]["ticket"], "widgets--01-foundation");
    assert_eq!(value["tickets"][0]["ready"], true);
}

/// An empty backlog still says which lane was searched and under which label, so
/// "nothing is filed" distinguishes "looked in the wrong place".
#[test]
fn ticket_list_explains_an_empty_backlog() {
    let (repository, config_home) = jj_repository();

    let output = run_ticket_list(repository.path(), config_home.path(), &["--json"]);

    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("valid JSON");
    assert_eq!(value["tickets"], serde_json::json!([]));
    assert_eq!(value["lanes"][0]["label"], "ready-for-agent");
}

/// The lanes are the lanes a `dispatch --all` would fill, and a Ticket is ready
/// when the lane it routes to would take it. A Ticket no lane's label matches is
/// blocked, not silently dropped.
#[test]
fn ticket_list_evaluates_every_routable_lane() {
    let (repository, config_home) = jj_repository();
    let directory = repository.path().join(".jjfx");
    fs::create_dir_all(&directory).expect("repository config directory");
    fs::write(
        directory.join("config.toml"),
        "[pools.bugs]\nsize = 1\nlabel = \"bug\"\n\n\
         [pools.features]\nsize = 1\nlabel = \"features\"\n",
    )
    .expect("repository config");
    write_ticket(
        repository.path(),
        "widgets",
        "01-bug.md",
        "# Bug\n\nStatus: ready-for-agent\nLabels: bug\n",
    );
    write_ticket(
        repository.path(),
        "widgets",
        "02-feature.md",
        "# Feature\n\nStatus: ready-for-agent\nLabels: features\n",
    );
    write_ticket(
        repository.path(),
        "widgets",
        "03-unrouted.md",
        "# Unrouted\n\nStatus: ready-for-agent\nLabels: ui\n",
    );
    write_ticket(
        repository.path(),
        "widgets",
        "04-blocked.md",
        "# Blocked\n\nStatus: ready-for-agent\nLabels: bug\nBlocked by: 01\n",
    );

    let output = run_ticket_list(repository.path(), config_home.path(), &["--json"]);

    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("valid JSON");
    let lanes = value["lanes"].as_array().expect("the lanes evaluated");
    let labels = lanes
        .iter()
        .map(|lane| lane["label"].as_str().expect("a label"))
        .collect::<Vec<_>>();
    assert_eq!(labels, ["bug", "features"]);

    let tickets = value["tickets"].as_array().expect("an array of Tickets");
    let ticket = |id: &str| {
        tickets
            .iter()
            .find(|ticket| ticket["ticket"] == id)
            .unwrap_or_else(|| panic!("no entry for {id}: {value}"))
    };
    // Each Ticket is ready because the lane its label routes to would take it,
    // even though the other lane's label gate excludes it.
    assert_eq!(ticket("widgets--01-bug")["state"], "ready");
    assert_eq!(ticket("widgets--02-feature")["state"], "ready");
    // A Ticket no lane's label matches is blocked, with the first lane's reason.
    let unrouted = ticket("widgets--03-unrouted");
    assert_eq!(unrouted["state"], "blocked");
    assert!(
        unrouted["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("missing label \"bug\"")),
        "{unrouted}"
    );
    // A blocked Ticket stays blocked in every lane, naming its Blocker.
    assert_eq!(ticket("widgets--04-blocked")["state"], "blocked");
    assert_eq!(
        ticket("widgets--04-blocked")["reason"],
        "blocked by widgets--01-bug (ready-for-agent)"
    );
}
