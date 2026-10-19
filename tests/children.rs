//! `jjfx children` at the surface a parent actually reaches: the built binary,
//! driven the way a session's shell would drive it.
//!
//! A parent runs this from inside an opencode session, so the tests set
//! `OPENCODE_SESSION_ID` (what opencode injects into that session's shell) or
//! name a session outright, point `XDG_STATE_HOME` at a lifecycle log, and read
//! what the command prints.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

/// The environment variable opencode injects into a session's shell commands.
const SESSION_ENV: &str = "OPENCODE_SESSION_ID";

/// One versioned lifecycle envelope, as the opencode plugin writes it.
fn event(name: &str, session_id: &str, parent_id: Option<&str>) -> String {
    let parent = parent_id.map_or(String::new(), |parent| format!("\"{parent}\""));
    format!(
        r#"{{"jjfx_event_version":1,"agent_kind":"opencode","session_id":"{session_id}","parent_id":{parent},"cwd":"/w/a","hook_event_name":"{name}"}}"#
    )
}

/// A global state directory holding `events.jsonl`, so the command folds a
/// lifecycle log the test controls.
fn state_home(events: &[String]) -> TempDir {
    let home = TempDir::new().expect("state directory");
    let log = home.path().join("jjfx");
    fs::create_dir_all(&log).expect("jjfx state directory");
    let mut contents = events.join("\n");
    contents.push('\n');
    fs::write(log.join("events.jsonl"), contents).expect("lifecycle log");
    home
}

/// Runs the built `jjfx` with `PATH` carrying no `opencode`, so the command
/// answers from the lifecycle log alone.
fn run(state: &Path, session: Option<&str>, args: &[&str]) -> Output {
    // An empty directory inside the state home, so no `opencode` is found and
    // the temporary directory is cleaned up with the rest.
    let bin = state.join("bin");
    fs::create_dir_all(&bin).expect("empty bin directory");
    let mut command = Command::new(env!("CARGO_BIN_EXE_jjfx"));
    command
        .args(["children"])
        .args(args)
        .env("XDG_STATE_HOME", state)
        .env_remove(SESSION_ENV)
        .env("PATH", &bin);
    if let Some(session) = session {
        command.env(SESSION_ENV, session);
    }
    command.output().expect("jjfx children should run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A parent names its children, and each row carries the child's folded state.
#[test]
fn children_lists_a_parents_child_sessions_with_their_state() {
    let state = state_home(&[
        event("SessionStart", "ses_root", None),
        event("UserPromptSubmit", "ses_root", None),
        event("SessionStart", "ses_child_a", Some("ses_root")),
        event("UserPromptSubmit", "ses_child_a", Some("ses_root")),
        event("SessionStart", "ses_child_b", Some("ses_root")),
        event("Stop", "ses_child_b", Some("ses_root")),
    ]);

    let output = run(state.path(), Some("ses_root"), &[]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = stdout(&output);
    assert!(text.contains("ses_root"), "{text}");
    assert!(text.contains("ses_child_a"), "{text}");
    assert!(text.contains("ses_child_b"), "{text}");
    assert!(
        text.contains("working"),
        "the working child's state: {text}"
    );
    assert!(
        text.contains("waiting"),
        "the finished child's state: {text}"
    );
}

/// A parent reaches the command with no argument at all: opencode put its own
/// session id in the environment, so a bare `jjfx children` answers for it.
#[test]
fn a_bare_children_defaults_to_the_session_that_ran_it() {
    let state = state_home(&[
        event("SessionStart", "ses_root", None),
        event("SessionStart", "ses_child_a", Some("ses_root")),
        event("UserPromptSubmit", "ses_child_a", Some("ses_root")),
    ]);

    let output = run(state.path(), Some("ses_root"), &[]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout(&output).contains("ses_child_a"),
        "{}",
        stdout(&output)
    );
}

/// A named session that dispatched nothing reads as nothing, not as an error.
#[test]
fn a_parent_with_no_children_reports_none() {
    let state = state_home(&[event("SessionStart", "ses_root", None)]);

    let output = run(state.path(), Some("ses_root"), &[]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout(&output).contains("no child sessions"),
        "{}",
        stdout(&output)
    );
}

/// Without a session named and nothing in the environment there is no parent to
/// ask about, and the refusal names what is missing.
#[test]
fn children_without_a_parent_names_the_requirement() {
    let state = state_home(&[]);

    let output = run(state.path(), None, &[]);

    assert!(!output.status.success(), "{}", stdout(&output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(SESSION_ENV), "{stderr}");
    assert!(stderr.contains("children"), "{stderr}");
}

/// `--json` is the shape a parent collects: one object naming the parent, where
/// the answer came from, and every child with its state.
#[test]
fn children_json_carries_every_child_and_its_state() {
    let state = state_home(&[
        event("SessionStart", "ses_root", None),
        event("SessionStart", "ses_child_a", Some("ses_root")),
        event("UserPromptSubmit", "ses_child_a", Some("ses_root")),
    ]);

    let output = run(state.path(), Some("ses_root"), &["--json"]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("children --json emits JSON");
    assert_eq!(value["parent"], "ses_root");
    assert_eq!(value["source"], "log");
    assert_eq!(value["children"][0]["session"], "ses_child_a");
    assert_eq!(value["children"][0]["state"], "working");
}

/// When opencode answers, it is the authority on which sessions are children,
/// and it supplies the title that says which Worker each one is.
#[test]
fn children_uses_opencode_as_the_authority_and_its_titles() {
    let state = state_home(&[
        event("SessionStart", "ses_root", None),
        event("SessionStart", "ses_child_a", Some("ses_root")),
        event("UserPromptSubmit", "ses_child_a", Some("ses_root")),
    ]);
    let bin = TempDir::new().expect("fake opencode directory");
    let script = bin.path().join("opencode");
    fs::write(
        &script,
        "#!/bin/sh\nprintf '%s' '{\"data\":[{\"id\":\"ses_child_a\",\"parentID\":\"ses_root\",\"title\":\"pool:worker-01:ENG-42\",\"location\":{\"directory\":\"/w/a\"}},{\"id\":\"ses_child_c\",\"parentID\":\"ses_root\"}]}'\n",
    )
    .expect("fake opencode");
    let mut permissions = fs::metadata(&script)
        .expect("script metadata")
        .permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    fs::set_permissions(&script, permissions).expect("make fake opencode executable");

    let output = Command::new(env!("CARGO_BIN_EXE_jjfx"))
        .args(["children", "ses_root", "--json"])
        .env("XDG_STATE_HOME", state.path())
        .env("PATH", bin.path())
        .output()
        .expect("jjfx children should run");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("children --json emits JSON");
    assert_eq!(value["source"], "opencode");
    // The log's child keeps its folded state and gains opencode's title.
    assert_eq!(value["children"][0]["session"], "ses_child_a");
    assert_eq!(value["children"][0]["state"], "working");
    assert_eq!(value["children"][0]["title"], "pool:worker-01:ENG-42");
    assert_eq!(value["children"][0]["directory"], "/w/a");
    // A child opencode knows but the log has not seen is listed, absent rather
    // than guessed.
    assert_eq!(value["children"][1]["session"], "ses_child_c");
    assert_eq!(value["children"][1]["state"], "absent");
}
