use std::fs;
use std::path::Path;
use std::process::Command;

use jjfx::engine::{
    LocalMarkdownQuery, LocalTickets, ReadyTicketFilter, TicketDiscovery, TicketId, TicketStatus,
};
use tempfile::TempDir;

fn write_ticket(root: &Path, feature: &str, file: &str, content: &str) {
    let directory = root.join(".scratch").join(feature).join("issues");
    fs::create_dir_all(&directory).expect("issues directory");
    fs::write(directory.join(file), content).expect("Ticket file");
}

fn source(root: &Path) -> LocalMarkdownQuery {
    LocalMarkdownQuery::new(root.join(".scratch"), "ready-for-agent", "resolved")
}

fn ready_filter() -> ReadyTicketFilter {
    ReadyTicketFilter::new(
        "ready-for-agent",
        TicketStatus::parse("ready-for-agent").expect("ready status"),
    )
    .expect("Ready Ticket filter")
}

/// A `jj` repository plus a `[tickets] source = "local"` config home, the shape
/// the CLI needs to reconcile local Tickets.
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

/// Append one succeeded Run Record for `ticket`, the durable completion signal a
/// local Ticket writeback resolves from - and the only signal left after `pool
/// reset` clears a Worker's Ticket association.
fn write_run_record(root: &Path, ticket: &str) {
    let pool = root.join(".jj").join("pool");
    fs::create_dir_all(&pool).expect("pool directory");
    let line = format!(
        "{{\"run_id\":\"worker-1@2026-01-01T00:00:00Z\",\"worker\":\"worker-1\",\
         \"ticket\":\"{ticket}\",\"conclusion\":\"succeeded\"}}\n"
    );
    fs::write(pool.join("runs.jsonl"), line).expect("run record");
}

fn ticket_status(root: &Path, feature: &str, file: &str) -> String {
    fs::read_to_string(
        root.join(".scratch")
            .join(feature)
            .join("issues")
            .join(file),
    )
    .expect("Ticket file")
}

#[test]
fn a_local_source_discovers_ready_tickets_and_inlines_the_body() {
    let repository = TempDir::new().expect("temporary repository");
    write_ticket(
        repository.path(),
        "widgets",
        "01-ship-the-widget.md",
        "# Ship the widget\n\nStatus: ready-for-agent\nType: task\nBlocked by: none\n\n## Problem\n\nInline this body.\n",
    );
    write_ticket(
        repository.path(),
        "widgets",
        "02-blocked.md",
        "# Blocked\n\nStatus: ready-for-agent\nBlocked by: 01\n",
    );
    write_ticket(
        repository.path(),
        "widgets",
        "03-done.md",
        "# Done\n\nStatus: resolved\n",
    );
    let discovery = TicketDiscovery::new(source(repository.path()));

    let ready = discovery.ready_tickets(&ready_filter()).expect("discovery");

    // 01 is ready; 02 is blocked by an unresolved sibling; 03 is resolved.
    assert_eq!(ready.tickets().len(), 1, "{:?}", ready.diagnostics());
    assert_eq!(
        ready.tickets()[0].id().as_str(),
        "widgets--01-ship-the-widget"
    );

    let id = TicketId::parse("widgets--01-ship-the-widget").expect("Ticket id");
    let details = discovery
        .ticket_details(&id)
        .expect("Ticket details")
        .expect("a Ticket file has details");
    assert_eq!(details.title().as_str(), "Ship the widget");
    assert_eq!(details.description(), "## Problem\n\nInline this body.");
}

#[test]
fn a_feature_parent_has_no_details_of_its_own() {
    // A local feature Parent is a directory, not a Ticket file: its id must
    // resolve to an absence rather than the hard "not found" error that used to
    // stop orchestration before it could enumerate the feature's children.
    let repository = TempDir::new().expect("temporary repository");
    write_ticket(
        repository.path(),
        "widgets",
        "01-ship-the-widget.md",
        "# Ship the widget\n\nStatus: ready-for-agent\n",
    );
    let discovery = TicketDiscovery::new(source(repository.path()));

    let parent = TicketId::parse("widgets").expect("Parent id");
    let details = discovery
        .ticket_details(&parent)
        .expect("a feature Parent is not a query failure");
    assert!(details.is_none(), "{details:?}");

    // An id that is neither a Ticket nor a feature is still a hard error, so a
    // mistyped Ticket is not silently dispatched without its body.
    let typo = TicketId::parse("nope-typo").expect("typo id");
    let error = discovery
        .ticket_details(&typo)
        .expect_err("an unknown id must still fail");
    assert!(error.to_string().contains("nope-typo"), "{error}");
}

#[test]
fn a_local_ticket_resolves_and_records_a_dated_comment() {
    let repository = TempDir::new().expect("temporary repository");
    write_ticket(
        repository.path(),
        "widgets",
        "01-ship-the-widget.md",
        "# Ship the widget\n\nStatus: claimed\n\n## Problem\n\nDo it.\n",
    );
    let tickets = LocalTickets::new(
        repository.path().join(".scratch"),
        "ready-for-agent",
        "resolved",
    );

    tickets
        .resolve("widgets--01-ship-the-widget")
        .expect("resolve");

    let rewritten = fs::read_to_string(
        repository
            .path()
            .join(".scratch/widgets/issues/01-ship-the-widget.md"),
    )
    .expect("rewritten Ticket");
    assert!(rewritten.contains("Status: resolved"), "{rewritten}");
    assert!(rewritten.contains("## Comments"), "{rewritten}");
    assert!(
        rewritten.contains("Resolved by jjfx dispatch."),
        "{rewritten}"
    );
}

#[test]
fn dispatch_all_with_a_local_source_discovers_from_scratch() {
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
    write_ticket(
        repository.path(),
        "widgets",
        "01-ship-the-widget.md",
        "# Ship the widget\n\nStatus: ready-for-agent\n\n## Problem\n\nDo it.\n",
    );
    write_ticket(
        repository.path(),
        "widgets",
        "02-no-title.md",
        "Status: ready-for-agent\n\nA body with no heading.\n",
    );
    let config_home = TempDir::new().expect("temporary config home");
    let config_directory = config_home.path().join("jjfx");
    fs::create_dir_all(&config_directory).expect("config directory");
    fs::write(
        config_directory.join("config.toml"),
        "[tickets]\nsource = \"local\"\n",
    )
    .expect("config file");

    let output = Command::new(env!("CARGO_BIN_EXE_jjfx"))
        .args(["dispatch", "--all", "--force"])
        .current_dir(repository.path())
        .env("XDG_CONFIG_HOME", config_home.path())
        .output()
        .expect("jjfx should run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Fetching tickets with label 'ready-for-agent'"),
        "{stderr}"
    );
    // The malformed file is surfaced, not silently skipped.
    assert!(stderr.contains("widgets--02-no-title"), "{stderr}");
    // A ready Ticket was found, so discovery did not collapse to an empty set.
    assert!(!stderr.contains("No tickets found"), "{stderr}");
}

#[test]
fn dispatch_all_json_reports_an_empty_batch_as_an_empty_array() {
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

    let output = Command::new(env!("CARGO_BIN_EXE_jjfx"))
        .args(["dispatch", "--all", "--json", "--force"])
        .current_dir(repository.path())
        .env("XDG_CONFIG_HOME", config_home.path())
        .output()
        .expect("jjfx should run");

    assert!(output.status.success(), "{:?}", output);
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "[]");
}

#[test]
fn dispatch_all_json_explains_an_empty_ready_set() {
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

    // Nothing is ready: 01 is in flight and 02 is blocked by it. The batch must
    // not read as a bare `[]` - each Ticket it skipped and why is in the JSON.
    let output = Command::new(env!("CARGO_BIN_EXE_jjfx"))
        .args(["dispatch", "--all", "--json", "--force"])
        .current_dir(repository.path())
        .env("XDG_CONFIG_HOME", config_home.path())
        .output()
        .expect("jjfx should run");

    assert!(output.status.success(), "{:?}", output);
    let value: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).expect("valid JSON");
    let entries = value.as_array().expect("an array of skipped entries");
    let skipped = |ticket: &str| {
        entries
            .iter()
            .find(|entry| entry["ticket"] == ticket)
            .unwrap_or_else(|| panic!("no skipped entry for {ticket}: {value}"))
    };
    assert_eq!(skipped("widgets--01-foundation")["outcome"], "skipped");
    assert_eq!(
        skipped("widgets--02-dependent")["detail"],
        "blocked by widgets--01-foundation (claimed)"
    );
}

#[test]
fn dispatch_all_prose_explains_an_empty_ready_set() {
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

    let output = Command::new(env!("CARGO_BIN_EXE_jjfx"))
        .args(["dispatch", "--all", "--force"])
        .current_dir(repository.path())
        .env("XDG_CONFIG_HOME", config_home.path())
        .output()
        .expect("jjfx should run");

    assert!(output.status.success(), "{:?}", output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No ready tickets with label 'ready-for-agent'"),
        "{stderr}"
    );
    assert!(
        stderr.contains("widgets--02-dependent blocked by widgets--01-foundation (claimed)"),
        "{stderr}"
    );
}

#[test]
fn dispatch_all_with_an_empty_ready_set_reconciles_finished_tickets() {
    let (repository, config_home) = jj_repository();
    write_ticket(
        repository.path(),
        "widgets",
        "01-foundation.md",
        "# Foundation\n\nStatus: claimed\n",
    );
    write_run_record(repository.path(), "widgets--01-foundation");

    // No ready Ticket and no live Worker: the only thing a Dispatch can do is
    // reconcile. Before the fix it returned `[]` and left the Ticket `claimed`.
    let output = Command::new(env!("CARGO_BIN_EXE_jjfx"))
        .args(["dispatch", "--all", "--json", "--force"])
        .current_dir(repository.path())
        .env("XDG_CONFIG_HOME", config_home.path())
        .output()
        .expect("jjfx should run");

    assert!(output.status.success(), "{:?}", output);
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "[]");
    assert!(
        ticket_status(repository.path(), "widgets", "01-foundation.md")
            .contains("Status: resolved"),
        "the finished Run must resolve its Ticket without a second Dispatch"
    );
}

#[test]
fn ticket_reconcile_resolves_a_finished_run_without_a_dispatch() {
    let (repository, config_home) = jj_repository();
    write_ticket(
        repository.path(),
        "widgets",
        "01-foundation.md",
        "# Foundation\n\nStatus: claimed\n",
    );
    write_run_record(repository.path(), "widgets--01-foundation");

    let output = Command::new(env!("CARGO_BIN_EXE_jjfx"))
        .args(["ticket", "reconcile"])
        .current_dir(repository.path())
        .env("XDG_CONFIG_HOME", config_home.path())
        .output()
        .expect("jjfx should run");

    assert!(output.status.success(), "{:?}", output);
    assert!(
        ticket_status(repository.path(), "widgets", "01-foundation.md")
            .contains("Status: resolved"),
        "an explicit reconcile must resolve a finished Run"
    );
}

#[test]
fn a_reconciled_blocker_readies_its_dependent_in_the_same_dispatch() {
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
    write_run_record(repository.path(), "widgets--01-foundation");

    // The reconcile before discovery resolves 01, so 02 becomes ready in the
    // same Dispatch (which then fails on the absent Pool - the readiness is what
    // this asserts).
    let output = Command::new(env!("CARGO_BIN_EXE_jjfx"))
        .args(["dispatch", "--all", "--json", "--force"])
        .current_dir(repository.path())
        .env("XDG_CONFIG_HOME", config_home.path())
        .output()
        .expect("jjfx should run");

    assert!(
        ticket_status(repository.path(), "widgets", "01-foundation.md")
            .contains("Status: resolved"),
        "the blocker must resolve"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("No tickets found"),
        "the dependent must be discovered as ready, not the ready set read empty: {stderr}"
    );
    let discovery = TicketDiscovery::new(source(repository.path()));
    let ready = discovery.ready_tickets(&ready_filter()).expect("discovery");
    assert_eq!(ready.tickets().len(), 1, "{:?}", ready.diagnostics());
    assert_eq!(ready.tickets()[0].id().as_str(), "widgets--02-dependent");
}
