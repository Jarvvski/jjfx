//! `jjfx hooks install` / `jjfx hooks status`: manage the dumb append-only hook
//! in `~/.claude/settings.json` (ADR 0002/0004). The hook is a dependency-free
//! shell append - no jjfx binary at hook time - so the config is written once
//! and never revised when the lifecycle logic changes. All state-machine logic
//! lives in the Rust binary (see `agent.rs`).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use serde_json::{Value, json};

use crate::events;

/// The lifecycle events the hook registers on - the deterministic set confirmed
/// by spike 01. The same append command serves every one; the payload carries
/// `hook_event_name`, so the fold discriminates, not the config.
const EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "Stop",
    "SessionEnd",
    "PermissionRequest",
];

/// Substring that identifies a jjfx-installed hook command, for idempotent
/// install and status checks.
const MARKER: &str = "jjfx/events.jsonl";

/// The dumb append command. It resolves the XDG state dir at hook time (matching
/// [`events::log_path`]), and writes exactly one line via `printf '%s\n'
/// "$(cat)"` - a single `O_APPEND` write below `PIPE_BUF`, so concurrent agents
/// never interleave (ADR 0004). Command substitution strips any trailing newline
/// on stdin, guaranteeing one JSONL line regardless.
pub fn hook_command() -> String {
    let dir = "${XDG_STATE_HOME:-$HOME/.local/state}/jjfx";
    format!("mkdir -p \"{dir}\" && printf '%s\\n' \"$(cat)\" >> \"{dir}/events.jsonl\"")
}

/// Path of the global Claude Code settings file (`~/.claude/settings.json`).
pub fn settings_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".claude").join("settings.json")
}

/// Outcome of an install: how many event hooks were newly added vs already
/// present (idempotency is observable, not silent).
#[derive(Debug, PartialEq, Eq)]
pub struct InstallOutcome {
    pub added: Vec<String>,
    pub already: Vec<String>,
}

/// Whether the jjfx hook is present for each event.
#[derive(Debug, PartialEq, Eq)]
pub struct StatusReport {
    pub installed: Vec<String>,
    pub missing: Vec<String>,
    pub log: PathBuf,
}

/// Install (idempotently) the jjfx hook for every lifecycle event, preserving
/// all other settings and hooks. Safe to run repeatedly.
pub fn install() -> anyhow::Result<InstallOutcome> {
    let path = settings_path();
    let mut root = read_settings(&path)?;
    let outcome = merge_hooks(&mut root, &hook_command())?;
    write_settings(&path, &root)?;
    Ok(outcome)
}

/// Report which events have the jjfx hook and which do not.
pub fn status() -> anyhow::Result<StatusReport> {
    let path = settings_path();
    let root = read_settings(&path)?;
    let hooks = root.get("hooks").and_then(Value::as_object);
    let (mut installed, mut missing) = (Vec::new(), Vec::new());
    for ev in EVENTS {
        let present = hooks
            .and_then(|h| h.get(*ev))
            .and_then(Value::as_array)
            .is_some_and(|arr| array_has_marker(arr));
        if present {
            installed.push((*ev).to_string());
        } else {
            missing.push((*ev).to_string());
        }
    }
    Ok(StatusReport {
        installed,
        missing,
        log: events::log_path(),
    })
}

/// Read `settings.json` into a JSON value, defaulting to an empty object when the
/// file is missing or blank. A present-but-invalid file is an error, not a
/// silent overwrite (never clobber a file we could not parse).
fn read_settings(path: &Path) -> anyhow::Result<Value> {
    match fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => {
            serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))
        }
        Ok(_) => Ok(Value::Object(Default::default())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Object(Default::default())),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Ensure each event's hook array contains a jjfx command entry, adding it only
/// where absent. Mutates `root` in place; returns what changed.
fn merge_hooks(root: &mut Value, command: &str) -> anyhow::Result<InstallOutcome> {
    let obj = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("settings.json is not a JSON object"))?;
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Default::default()))
        .as_object_mut()
        .ok_or_else(|| anyhow!(".hooks is not a JSON object"))?;

    let mut outcome = InstallOutcome {
        added: Vec::new(),
        already: Vec::new(),
    };
    for ev in EVENTS {
        let arr = hooks
            .entry((*ev).to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| anyhow!(".hooks.{ev} is not an array"))?;
        if array_has_marker(arr) {
            outcome.already.push((*ev).to_string());
        } else {
            arr.push(json!({ "hooks": [ { "type": "command", "command": command } ] }));
            outcome.added.push((*ev).to_string());
        }
    }
    Ok(outcome)
}

/// Does any hook group in this event's array carry a jjfx command?
fn array_has_marker(arr: &[Value]) -> bool {
    arr.iter().any(|group| {
        group
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|inner| {
                inner.iter().any(|h| {
                    h.get("command")
                        .and_then(Value::as_str)
                        .is_some_and(|c| c.contains(MARKER))
                })
            })
    })
}

/// Write settings atomically (temp file in the same dir, then rename), pretty and
/// newline-terminated, so a crash mid-write never corrupts the user's config.
fn write_settings(path: &Path, root: &Value) -> anyhow::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("settings path has no parent"))?;
    fs::create_dir_all(dir)?;
    let mut text = serde_json::to_string_pretty(root)?;
    text.push('\n');
    let tmp = dir.join(format!("settings.json.{}.tmp", std::process::id()));
    fs::write(&tmp, text.as_bytes())?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Dispatch the `hooks` subcommand. `None` or `status` reports; `install`
/// installs.
pub fn run_cli(sub: Option<&str>) -> anyhow::Result<()> {
    match sub {
        Some("install") => {
            let outcome = install()?;
            if outcome.added.is_empty() {
                println!(
                    "jjfx hooks already installed for all {} events.",
                    outcome.already.len()
                );
            } else {
                println!("Installed jjfx hook for: {}", outcome.added.join(", "));
                if !outcome.already.is_empty() {
                    println!("Already present for: {}", outcome.already.join(", "));
                }
            }
            println!("Events log: {}", events::log_path().display());
        }
        None | Some("status") => {
            let report = status()?;
            if report.missing.is_empty() {
                println!("Hooks installed for all {} events.", report.installed.len());
            } else {
                println!("Installed: {}", join_or_none(&report.installed));
                println!("Missing:   {}", join_or_none(&report.missing));
                println!("Run `jjfx hooks install` to add the missing hooks.");
            }
            println!("Events log: {}", report.log.display());
        }
        Some(other) => bail!("unknown hooks subcommand: {other} (try `install` or `status`)"),
    }
    Ok(())
}

fn join_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "(none)".to_string()
    } else {
        items.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_targets_the_log_and_writes_one_line() {
        let cmd = hook_command();
        assert!(cmd.contains(MARKER));
        assert!(cmd.contains("printf '%s\\n' \"$(cat)\""));
        assert!(cmd.contains(">>"));
    }

    #[test]
    fn merge_into_empty_adds_all_events() {
        let mut root = Value::Object(Default::default());
        let outcome = merge_hooks(&mut root, "CMD").unwrap();
        assert_eq!(outcome.added.len(), EVENTS.len());
        assert!(outcome.already.is_empty());
        // Every event array now carries a group with our command.
        let hooks = root["hooks"].as_object().unwrap();
        for ev in EVENTS {
            let arr = hooks[*ev].as_array().unwrap();
            assert_eq!(arr[0]["hooks"][0]["command"], "CMD");
        }
    }

    #[test]
    fn merge_is_idempotent() {
        let mut root = Value::Object(Default::default());
        merge_hooks(&mut root, &hook_command()).unwrap();
        let second = merge_hooks(&mut root, &hook_command()).unwrap();
        assert!(second.added.is_empty());
        assert_eq!(second.already.len(), EVENTS.len());
        // No duplicate groups were appended.
        let arr = root["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
    }

    #[test]
    fn merge_preserves_existing_hooks_and_keys() {
        let mut root = json!({
            "model": "opus",
            "hooks": {
                "Stop": [ { "hooks": [ { "type": "command", "command": "existing" } ] } ],
                "PreToolUse": [ { "matcher": "Bash", "hooks": [ { "type": "command", "command": "lint" } ] } ]
            }
        });
        merge_hooks(&mut root, "CMD").unwrap();
        // Unrelated top-level key untouched.
        assert_eq!(root["model"], "opus");
        // Pre-existing Stop hook kept; jjfx one appended alongside it.
        let stop = root["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "existing");
        assert_eq!(stop[1]["hooks"][0]["command"], "CMD");
        // A non-lifecycle event we do not manage is left entirely alone.
        let pre = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0]["hooks"][0]["command"], "lint");
    }

    #[test]
    fn array_has_marker_detects_our_command() {
        let arr = vec![json!({ "hooks": [ { "type": "command", "command": hook_command() } ] })];
        assert!(array_has_marker(&arr));
        let other = vec![json!({ "hooks": [ { "type": "command", "command": "echo hi" } ] })];
        assert!(!array_has_marker(&other));
    }
}
