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
    let details = discovery.ticket_details(&id).expect("Ticket details");
    assert_eq!(details.title().as_str(), "Ship the widget");
    assert_eq!(details.description(), "## Problem\n\nInline this body.");
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
