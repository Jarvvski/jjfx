//! `jjfx hooks install` / `jjfx hooks status`: manage lifecycle integrations
//! for Claude Code, Codex, and OpenCode (ADR 0002/0004). Claude and Codex use
//! dumb append-only JSON hooks; OpenCode uses a jjfx-owned plugin. Every source
//! normalizes into the same log contract, and state-machine logic remains in
//! Rust (see `agent.rs`).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use serde_json::{Value, json};

use crate::events;

/// The lifecycle events the hook registers on for Claude Code - the
/// deterministic set confirmed by spike 01 plus the richer 2.x signals jjfx now
/// folds (ADR 0002). The same append command serves every one; the payload
/// carries `hook_event_name` (and, for the catch-alls, `notification_type` /
/// `error`), so the fold discriminates, not the config.
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
    // The catch-all notification stream; `notification_type` discriminates
    // `agent_needs_input` from the rest (see agent.rs).
    "Notification",
    // A turn that ended on an API error; `error` discriminates a quota/billing
    // wall (`rate_limit` / `billing_error`) from an ordinary failure.
    "StopFailure",
    // Live progress for long Runs: subagent spawn/finish, tool-batch
    // resolution, and task/teammate transitions. Recorded, never a state move.
    "SubagentStart",
    "SubagentStop",
    "PostToolBatch",
    "TaskCreated",
    "TaskCompleted",
    "TeammateIdle",
];

/// Codex supports the same event names and payload shape minus `SessionEnd`,
/// so a closed codex session stays `waiting` after its final Stop rather than
/// reaching `ended`. Codex's own `Interrupt` and subagent events are added
/// alongside.
const CODEX_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "Stop",
    "PermissionRequest",
    "PostToolUse",
    "Interrupt",
    "SubagentStart",
    "SubagentStop",
];

/// One hooks file jjfx manages: whose it is, where it lives, and which
/// lifecycle events it registers.
struct Target {
    agent: &'static str,
    path: PathBuf,
    events: &'static [&'static str],
}

/// The hooks files jjfx installs into - both agents unconditionally, so changing
/// `[agent] command` needs no reinstall. The append hook is inert for an agent
/// that is never run.
fn targets(paths: &IntegrationPaths) -> Vec<Target> {
    vec![
        Target {
            agent: "claude",
            path: paths.claude_settings.clone(),
            events: CLAUDE_EVENTS,
        },
        Target {
            agent: "codex",
            path: paths.codex_hooks.clone(),
            events: CODEX_EVENTS,
        },
    ]
}

/// Substring that identifies a jjfx-installed hook command, for idempotent
/// install and status checks.
const MARKER: &str = "jjfx/events.jsonl";
const OPENCODE_PLUGIN_SOURCE: &str = include_str!("../assets/opencode/jjfx-lifecycle.ts");
const OPENCODE_PLUGIN_MARKER: &str = "// jjfx-opencode-lifecycle-plugin:";
const OPENCODE_PLUGIN_NAME: &str = "lifecycle plugin";

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

fn opencode_plugin_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| home_dir().join(".config"))
        .join("opencode/plugins/jjfx-lifecycle.ts")
}

struct IntegrationPaths {
    claude_settings: PathBuf,
    codex_hooks: PathBuf,
    opencode_plugin: PathBuf,
}

impl IntegrationPaths {
    fn from_env() -> Self {
        Self {
            claude_settings: claude_settings_path(),
            codex_hooks: codex_hooks_path(),
            opencode_plugin: opencode_plugin_path(),
        }
    }
}

/// Outcome of installing one agent's lifecycle integration. Added, upgraded,
/// and already-current resources remain separately observable.
#[derive(Debug, PartialEq, Eq)]
pub struct InstallOutcome {
    /// Agent whose integration was inspected.
    pub agent: &'static str,
    /// Lifecycle resources created by the installation.
    pub added: Vec<String>,
    /// Existing jjfx lifecycle resources upgraded by the installation.
    pub updated: Vec<String>,
    /// Lifecycle resources already matching the installed version.
    pub already: Vec<String>,
}

/// Installation state for one agent's lifecycle resources.
#[derive(Debug, PartialEq, Eq)]
pub struct StatusReport {
    /// Agent whose integration was inspected.
    pub agent: &'static str,
    /// Lifecycle resources matching the installed version.
    pub installed: Vec<String>,
    /// Lifecycle resources that have not been installed.
    pub missing: Vec<String>,
    /// Older jjfx lifecycle resources that can be upgraded safely.
    pub outdated: Vec<String>,
    /// Non-jjfx resources at paths owned by the integration.
    pub conflicting: Vec<String>,
}

/// Install (idempotently) the jjfx hook for every lifecycle event of every
/// agent, preserving all other settings and hooks. Safe to run repeatedly.
pub fn install() -> anyhow::Result<Vec<InstallOutcome>> {
    install_with_paths(&IntegrationPaths::from_env())
}

fn install_with_paths(paths: &IntegrationPaths) -> anyhow::Result<Vec<InstallOutcome>> {
    let command = hook_command();
    let mut outcomes: Vec<InstallOutcome> = targets(paths)
        .into_iter()
        .map(|target| {
            let mut root = read_settings(&target.path)?;
            let (added, already) = merge_hooks(&mut root, &command, target.events)?;
            write_settings(&target.path, &root)?;
            Ok(InstallOutcome {
                agent: target.agent,
                added,
                updated: Vec::new(),
                already,
            })
        })
        .collect::<anyhow::Result<_>>()?;
    outcomes.push(install_opencode_plugin(&paths.opencode_plugin)?);
    Ok(outcomes)
}

fn install_opencode_plugin(path: &Path) -> anyhow::Result<InstallOutcome> {
    install_owned_source(
        path,
        OPENCODE_PLUGIN_SOURCE,
        OPENCODE_PLUGIN_MARKER,
        "opencode",
        OPENCODE_PLUGIN_NAME,
    )
}

fn install_owned_source(
    path: &Path,
    source: &str,
    marker: &str,
    agent: &'static str,
    name: &str,
) -> anyhow::Result<InstallOutcome> {
    let current = match fs::read_to_string(path) {
        Ok(source) => Some(source),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let (added, updated, already) = match current.as_deref() {
        Some(existing) if existing == source => (Vec::new(), Vec::new(), vec![name.to_owned()]),
        Some(existing) if existing.contains(marker) => {
            (Vec::new(), vec![name.to_owned()], Vec::new())
        }
        Some(_) => bail!(
            "refusing to overwrite non-jjfx OpenCode plugin at {}",
            path.display()
        ),
        None => (vec![name.to_owned()], Vec::new(), Vec::new()),
    };
    if !already.is_empty() {
        return Ok(InstallOutcome {
            agent,
            added,
            updated,
            already,
        });
    }
    let directory = path
        .parent()
        .ok_or_else(|| anyhow!("OpenCode plugin path has no parent"))?;
    fs::create_dir_all(directory)?;
    fs::write(path, source)?;
    Ok(InstallOutcome {
        agent,
        added,
        updated,
        already,
    })
}

/// Report, per agent, which events have the jjfx hook and which do not.
pub fn status() -> anyhow::Result<Vec<StatusReport>> {
    status_with_paths(&IntegrationPaths::from_env())
}

fn status_with_paths(paths: &IntegrationPaths) -> anyhow::Result<Vec<StatusReport>> {
    let mut reports: Vec<StatusReport> = targets(paths)
        .into_iter()
        .map(|target| {
            let root = read_settings(&target.path)?;
            let hooks = root.get("hooks").and_then(Value::as_object);
            let (mut installed, mut missing) = (Vec::new(), Vec::new());
            for event in target.events {
                let present = hooks
                    .and_then(|entries| entries.get(*event))
                    .and_then(Value::as_array)
                    .is_some_and(|entries| array_has_marker(entries));
                if present {
                    installed.push((*event).to_owned());
                } else {
                    missing.push((*event).to_owned());
                }
            }
            Ok(StatusReport {
                agent: target.agent,
                installed,
                missing,
                outdated: Vec::new(),
                conflicting: Vec::new(),
            })
        })
        .collect::<anyhow::Result<_>>()?;
    reports.push(opencode_plugin_status(&paths.opencode_plugin)?);
    Ok(reports)
}

fn opencode_plugin_status(path: &Path) -> anyhow::Result<StatusReport> {
    let mut report = StatusReport {
        agent: "opencode",
        installed: Vec::new(),
        missing: Vec::new(),
        outdated: Vec::new(),
        conflicting: Vec::new(),
    };
    match fs::read_to_string(path) {
        Ok(source) if source == OPENCODE_PLUGIN_SOURCE => {
            report.installed.push(OPENCODE_PLUGIN_NAME.to_owned())
        }
        Ok(source) if source.contains(OPENCODE_PLUGIN_MARKER) => {
            report.outdated.push(OPENCODE_PLUGIN_NAME.to_owned())
        }
        Ok(_) => report.conflicting.push(OPENCODE_PLUGIN_NAME.to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            report.missing.push(OPENCODE_PLUGIN_NAME.to_owned())
        }
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    }
    Ok(report)
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
                if !outcome.added.is_empty() {
                    println!(
                        "{}: installed lifecycle integration for: {}",
                        outcome.agent,
                        outcome.added.join(", ")
                    );
                }
                if !outcome.updated.is_empty() {
                    println!(
                        "{}: updated lifecycle integration for: {}",
                        outcome.agent,
                        outcome.updated.join(", ")
                    );
                }
                if !outcome.already.is_empty() {
                    println!(
                        "{}: already present for: {}",
                        outcome.agent,
                        outcome.already.join(", ")
                    );
                }
            }
            println!("Events log: {}", events::log_path().display());
        }
        None | Some("status") => {
            let reports = status()?;
            let mut needs_install = false;
            for report in &reports {
                let current = report.missing.is_empty()
                    && report.outdated.is_empty()
                    && report.conflicting.is_empty();
                if current {
                    println!(
                        "{}: lifecycle integration current for all {} resources.",
                        report.agent,
                        report.installed.len()
                    );
                    continue;
                }

                needs_install |= !report.missing.is_empty() || !report.outdated.is_empty();
                println!(
                    "{}: installed:   {}",
                    report.agent,
                    join_or_none(&report.installed)
                );
                println!(
                    "{}: missing:     {}",
                    report.agent,
                    join_or_none(&report.missing)
                );
                println!(
                    "{}: outdated:    {}",
                    report.agent,
                    join_or_none(&report.outdated)
                );
                println!(
                    "{}: conflicting: {}",
                    report.agent,
                    join_or_none(&report.conflicting)
                );
            }
            if needs_install {
                println!("Run `jjfx hooks install` to add or update lifecycle integrations.");
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
    fn opencode_plugin_emits_the_versioned_lifecycle_contract() {
        for field in [
            "jjfx_event_version",
            "hook_event_name",
            "agent_kind: \"opencode\"",
            "session_id",
            "parent_id",
            "cwd",
        ] {
            assert!(
                OPENCODE_PLUGIN_SOURCE.contains(field),
                "missing plugin field {field}"
            );
        }
        assert!(OPENCODE_PLUGIN_SOURCE.contains("session.status"));
        assert!(OPENCODE_PLUGIN_SOURCE.contains("permission.asked"));
    }

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
    fn codex_registers_its_shared_and_own_events() {
        // Codex's hook set has no SessionEnd, so a closed codex session stays
        // waiting rather than reaching ended; it adds its own Interrupt and the
        // shared subagent events. The same fold in agent.rs serves both logs.
        assert!(!CODEX_EVENTS.contains(&"SessionEnd"));
        assert!(CODEX_EVENTS.contains(&"Interrupt"));
        assert!(CODEX_EVENTS.contains(&"SubagentStart"));
        assert!(CODEX_EVENTS.contains(&"SubagentStop"));
        // Everything else Codex registers is shared with Claude.
        for ev in CODEX_EVENTS {
            if *ev == "Interrupt" {
                continue;
            }
            assert!(CLAUDE_EVENTS.contains(ev), "{ev} unknown to claude");
        }
    }

    #[test]
    fn claude_registers_the_richer_lifecycle_signals() {
        // The catch-alls and progress events the fold now understands.
        for ev in [
            "Notification",
            "StopFailure",
            "SubagentStart",
            "SubagentStop",
            "PostToolBatch",
            "TaskCreated",
            "TaskCompleted",
            "TeammateIdle",
        ] {
            assert!(CLAUDE_EVENTS.contains(&ev), "claude is missing {ev}");
        }
    }

    #[test]
    fn targets_cover_both_agents_own_files() {
        let targets = targets(&IntegrationPaths::from_env());
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
