//! `jjfx resolve` against a real rebase conflict (ticket 13).
//!
//! jj records a conflict *in the commit* and a rebase that hits one succeeds, so
//! the tool has to be able to name that commit and resolve it - not treat a
//! conflicted chain as a forge failure it silently skips.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// Run one jj command in `dir`, asserting it succeeds.
fn jj(dir: &Path, args: &[&str]) {
    let output = Command::new("jj")
        .args(["--config", "signing.behavior=drop"])
        .current_dir(dir)
        .args(args)
        .output()
        .expect("jj should be installed");
    assert!(
        output.status.success(),
        "jj {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A repository whose `ws-a` chain holds a real rebase conflict: `ws-a` and
/// `ws-b` each rewrite the same line from the shared base, then `ws-a` is rebased
/// onto `ws-b`.
fn conflicted_repository() -> (TempDir, PathBuf, PathBuf) {
    let parent = TempDir::new().expect("temporary directory");
    let root = parent.path().join("repo");
    fs::create_dir_all(&root).expect("repository directory");
    let output = Command::new("jj")
        .args([
            "--config",
            "signing.behavior=drop",
            "git",
            "init",
            "--no-colocate",
        ])
        .arg(&root)
        .output()
        .expect("jj should be installed");
    assert!(
        output.status.success(),
        "jj init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    fs::write(root.join("f.txt"), "base\n").expect("write base file");
    jj(&root, &["commit", "-m", "base"]);
    jj(&root, &["bookmark", "create", "main", "-r", "@-"]);

    let ws_a = parent.path().join("ws-a");
    let ws_b = parent.path().join("ws-b");
    jj(
        &root,
        &[
            "workspace",
            "add",
            "--name",
            "ws-a",
            "--no-colocate",
            &ws_a.to_string_lossy(),
        ],
    );
    jj(
        &root,
        &[
            "workspace",
            "add",
            "--name",
            "ws-b",
            "--no-colocate",
            &ws_b.to_string_lossy(),
        ],
    );
    fs::write(ws_a.join("f.txt"), "a\n").expect("write a");
    jj(&ws_a, &["commit", "-m", "a change"]);
    fs::write(ws_b.join("f.txt"), "b\n").expect("write b");
    jj(&ws_b, &["commit", "-m", "b change"]);
    jj(&root, &["rebase", "-s", "\"ws-a\"@-", "-d", "\"ws-b\"@-"]);

    (parent, root, ws_a)
}

/// Run `jjfx` with the given arguments in `dir`, returning its output.
fn jjfx(dir: &Path, config_home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_jjfx"))
        .args(args)
        .current_dir(dir)
        .env("XDG_CONFIG_HOME", config_home)
        .output()
        .expect("jjfx should run")
}

#[test]
fn resolve_clears_a_real_rebase_conflict_and_then_reports_nothing_to_do() {
    let (_parent, _root, ws_a) = conflicted_repository();
    let config_home = TempDir::new().expect("temporary config home");

    let output = jjfx(
        &ws_a,
        config_home.path(),
        &["resolve", "ws-a", "--tool", ":ours"],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "resolve failed: {stderr}");
    assert!(
        stderr.contains("resolving 2 conflicted commit(s) on ws-a"),
        "the refusal names what it is about to resolve: {stderr}"
    );
    assert!(stderr.contains("resolved ws-a"), "{stderr}");

    // A one-sided resolve leaves one side whole, not a conflict marker.
    let content = fs::read_to_string(ws_a.join("f.txt")).expect("read resolved file");
    assert!(
        content == "a\n" || content == "b\n",
        "the conflict must be gone, got {content:?}"
    );

    // The chain is clean now, so a second run says so instead of opening a tool.
    let second = jjfx(&ws_a, config_home.path(), &["resolve", "ws-a"]);
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(second.status.success(), "second resolve failed: {stderr}");
    assert!(
        stderr.contains("ws-a has no conflicts to resolve"),
        "{stderr}"
    );
}
