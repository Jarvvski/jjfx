//! `jjfx hooks install` / `jjfx hooks status`: manage the dumb append-only hook
//! in `~/.claude/settings.json` and `~/.codex/hooks.json` (ADR 0002/0004). Both
//! agents use the same nested hooks JSON shape and emit the same payload fields,
//! so one merge serves both files. The hook is a dependency-free shell append -
//! no jjfx binary at hook time - so the config is written once and never revised
//! when the lifecycle logic changes. All state-machine logic lives in the Rust
//! binary (see `agent.rs`).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use serde_json::{Value, json};

use crate::events;

/// The lifecycle events the hook registers on for Claude Code - the
/// deterministic set confirmed by spike 01. The same append command serves
/// every one; the payload carries `hook_event_name`, so the fold discriminates,
/// not the config.
const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "Stop",
    "SessionEnd",
    "PermissionRequest",
    // No hook fires when a permission dialog is *resolved*; the first tool
    // completing afterwards is the observable "running again" signal that
    // clears needs-attention (see agent.rs). Chatty, but the log rotates.
    "PostToolUse",
];

/// Codex supports the same event names and payload shape minus `SessionEnd`,
/// so a closed codex session stays `waiting` after its final Stop rather than
/// reaching `ended`.
const CODEX_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "Stop",
    "PermissionRequest",
    "PostToolUse",
];

/// One hooks file jjfx manages: whose it is, where it lives, and which
/// lifecycle events it registers.
struct Target {
    agent: &'static str,
    path: PathBuf,
    events: &'static [&'static str],
}

/// The hooks files jjfx installs into - both agents unconditionally, so a later
/// `[terminal] agent` switch needs no reinstall. The append hook is inert for
/// an agent that is never run.
fn targets() -> Vec<Target> {
    vec![
        Target {
            agent: "claude",
            path: claude_settings_path(),
            events: CLAUDE_EVENTS,
        },
        Target {
            agent: "codex",
            path: codex_hooks_path(),
            events: CODEX_EVENTS,
        },
    ]
}

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
fn claude_settings_path() -> PathBuf {
    home_dir().join(".claude").join("settings.json")
}

/// Path of the global Codex hooks file (`~/.codex/hooks.json`). Codex also
/// accepts inline `[hooks]` tables in its `config.toml`; jjfx owns the JSON
/// file because it shares the shape of Claude's `hooks` settings block.
fn codex_hooks_path() -> PathBuf {
    home_dir().join(".codex").join("hooks.json")
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Outcome of an install into one agent's hooks file: how many event hooks were
/// newly added vs already present (idempotency is observable, not silent).
#[derive(Debug, PartialEq, Eq)]
pub struct InstallOutcome {
    pub agent: &'static str,
    pub added: Vec<String>,
    pub already: Vec<String>,
}

/// Whether the jjfx hook is present for each of one agent's events.
#[derive(Debug, PartialEq, Eq)]
pub struct StatusReport {
    pub agent: &'static str,
    pub installed: Vec<String>,
    pub missing: Vec<String>,
}

/// Install (idempotently) the jjfx hook for every lifecycle event of every
/// agent, preserving all other settings and hooks. Safe to run repeatedly.
pub fn install() -> anyhow::Result<Vec<InstallOutcome>> {
    let command = hook_command();
    targets()
        .into_iter()
        .map(|t| {
            let mut root = read_settings(&t.path)?;
            let (added, already) = merge_hooks(&mut root, &command, t.events)?;
            write_settings(&t.path, &root)?;
            Ok(InstallOutcome {
                agent: t.agent,
                added,
                already,
            })
        })
        .collect()
}

/// Report, per agent, which events have the jjfx hook and which do not.
pub fn status() -> anyhow::Result<Vec<StatusReport>> {
    targets()
        .into_iter()
        .map(|t| {
            let root = read_settings(&t.path)?;
            let hooks = root.get("hooks").and_then(Value::as_object);
            let (mut installed, mut missing) = (Vec::new(), Vec::new());
            for ev in t.events {
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
                agent: t.agent,
                installed,
                missing,
            })
        })
        .collect()
}

/// Read a hooks file into a JSON value, defaulting to an empty object when the
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
/// where absent. Mutates `root` in place; returns the (added, already-present)
/// event names.
fn merge_hooks(
    root: &mut Value,
    command: &str,
    events: &[&str],
) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let obj = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("hooks file is not a JSON object"))?;
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Default::default()))
        .as_object_mut()
        .ok_or_else(|| anyhow!(".hooks is not a JSON object"))?;

    let (mut added, mut already) = (Vec::new(), Vec::new());
    for ev in events {
        let arr = hooks
            .entry((*ev).to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| anyhow!(".hooks.{ev} is not an array"))?;
        if array_has_marker(arr) {
            already.push((*ev).to_string());
        } else {
            arr.push(json!({ "hooks": [ { "type": "command", "command": command } ] }));
            added.push((*ev).to_string());
        }
    }
    Ok((added, already))
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

/// Write a hooks file atomically (temp file in the same dir, then rename),
/// pretty and newline-terminated, so a crash mid-write never corrupts the
/// user's config.
fn write_settings(path: &Path, root: &Value) -> anyhow::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("hooks file path has no parent"))?;
    fs::create_dir_all(dir)?;
    let mut text = serde_json::to_string_pretty(root)?;
    text.push('\n');
    let tmp = dir.join(format!("hooks.{}.tmp", std::process::id()));
    fs::write(&tmp, text.as_bytes())?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Dispatch the `hooks` subcommand. `None` or `status` reports; `install`
/// installs.
pub fn run_cli(sub: Option<&str>) -> anyhow::Result<()> {
    match sub {
        Some("install") => {
            for outcome in install()? {
                if outcome.added.is_empty() {
                    println!(
                        "{}: jjfx hooks already installed for all {} events.",
                        outcome.agent,
                        outcome.already.len()
                    );
                } else {
                    println!(
                        "{}: installed jjfx hook for: {}",
                        outcome.agent,
                        outcome.added.join(", ")
                    );
                    if !outcome.already.is_empty() {
                        println!(
                            "{}: already present for: {}",
                            outcome.agent,
                            outcome.already.join(", ")
                        );
                    }
                }
            }
            println!("Events log: {}", events::log_path().display());
        }
        None | Some("status") => {
            let reports = status()?;
            let mut any_missing = false;
            for report in &reports {
                if report.missing.is_empty() {
                    println!(
                        "{}: hooks installed for all {} events.",
                        report.agent,
                        report.installed.len()
                    );
                } else {
                    any_missing = true;
                    println!(
                        "{}: installed: {}",
                        report.agent,
                        join_or_none(&report.installed)
                    );
                    println!(
                        "{}: missing:   {}",
                        report.agent,
                        join_or_none(&report.missing)
                    );
                }
            }
            if any_missing {
                println!("Run `jjfx hooks install` to add the missing hooks.");
            }
            println!("Events log: {}", events::log_path().display());
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
        let (added, already) = merge_hooks(&mut root, "CMD", CLAUDE_EVENTS).unwrap();
        assert_eq!(added.len(), CLAUDE_EVENTS.len());
        assert!(already.is_empty());
        // Every event array now carries a group with our command.
        let hooks = root["hooks"].as_object().unwrap();
        for ev in CLAUDE_EVENTS {
            let arr = hooks[*ev].as_array().unwrap();
            assert_eq!(arr[0]["hooks"][0]["command"], "CMD");
        }
    }

    #[test]
    fn merge_is_idempotent() {
        let mut root = Value::Object(Default::default());
        merge_hooks(&mut root, &hook_command(), CLAUDE_EVENTS).unwrap();
        let (added, already) = merge_hooks(&mut root, &hook_command(), CLAUDE_EVENTS).unwrap();
        assert!(added.is_empty());
        assert_eq!(already.len(), CLAUDE_EVENTS.len());
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
        merge_hooks(&mut root, "CMD", CLAUDE_EVENTS).unwrap();
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
    fn codex_registers_every_claude_event_except_session_end() {
        // Codex's hook set has no SessionEnd; everything else matches, so the
        // same fold in agent.rs serves both logs.
        assert!(!CODEX_EVENTS.contains(&"SessionEnd"));
        for ev in CODEX_EVENTS {
            assert!(CLAUDE_EVENTS.contains(ev), "{ev} unknown to claude");
        }
        assert_eq!(CODEX_EVENTS.len(), CLAUDE_EVENTS.len() - 1);
    }

    #[test]
    fn targets_cover_both_agents_own_files() {
        let targets = targets();
        let agents: Vec<_> = targets.iter().map(|t| t.agent).collect();
        assert_eq!(agents, ["claude", "codex"]);
        assert!(targets[0].path.ends_with(".claude/settings.json"));
        assert!(targets[1].path.ends_with(".codex/hooks.json"));
    }

    #[test]
    fn array_has_marker_detects_our_command() {
        let arr = vec![json!({ "hooks": [ { "type": "command", "command": hook_command() } ] })];
        assert!(array_has_marker(&arr));
        let other = vec![json!({ "hooks": [ { "type": "command", "command": "echo hi" } ] })];
        assert!(!array_has_marker(&other));
    }
}
