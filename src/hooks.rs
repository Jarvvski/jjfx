//! `jjfx hooks install` / `jjfx hooks status`: manage lifecycle integrations
//! for Claude Code, Codex, and Pi (ADR 0002/0004). Claude and Codex use dumb
//! append-only JSON hooks. Pi uses a jjfx-owned auto-discovered extension that
//! normalizes its lifecycle events into the same log contract. State-machine
//! logic remains in Rust (see `agent.rs`).

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
const PI_EXTENSION_SOURCE: &str = include_str!("../assets/pi/jjfx-lifecycle.ts");
const PI_EXTENSION_MARKER: &str = "// jjfx-pi-lifecycle-extension:";
const PI_EXTENSION_NAME: &str = "lifecycle extension";

fn pi_extension_source() -> &'static str {
    PI_EXTENSION_SOURCE
}

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

fn pi_extension_path() -> PathBuf {
    std::env::var_os("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| home_dir().join(".pi").join("agent"))
        .join("extensions")
        .join("jjfx-lifecycle.ts")
}

struct IntegrationPaths {
    claude_settings: PathBuf,
    codex_hooks: PathBuf,
    pi_extension: PathBuf,
}

impl IntegrationPaths {
    fn from_env() -> Self {
        Self {
            claude_settings: claude_settings_path(),
            codex_hooks: codex_hooks_path(),
            pi_extension: pi_extension_path(),
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
    let pi_status = pi_extension_status(&paths.pi_extension)?;
    if !pi_status.conflicting.is_empty() {
        bail!(
            "refusing to overwrite non-jjfx Pi extension at {}",
            paths.pi_extension.display()
        );
    }

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
    outcomes.push(install_pi_extension(&paths.pi_extension)?);
    Ok(outcomes)
}

fn install_pi_extension(path: &Path) -> anyhow::Result<InstallOutcome> {
    let current = match fs::read_to_string(path) {
        Ok(source) => Some(source),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };

    let (added, updated, already) = match current.as_deref() {
        Some(source) if source == pi_extension_source() => {
            (Vec::new(), Vec::new(), vec![PI_EXTENSION_NAME.to_owned()])
        }
        Some(source) if source.contains(PI_EXTENSION_MARKER) => {
            write_pi_extension(path)?;
            (Vec::new(), vec![PI_EXTENSION_NAME.to_owned()], Vec::new())
        }
        Some(_) => bail!(
            "refusing to overwrite non-jjfx Pi extension at {}",
            path.display()
        ),
        None => {
            write_pi_extension(path)?;
            (vec![PI_EXTENSION_NAME.to_owned()], Vec::new(), Vec::new())
        }
    };

    Ok(InstallOutcome {
        agent: "pi",
        added,
        updated,
        already,
    })
}

fn write_pi_extension(path: &Path) -> anyhow::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("Pi extension path has no parent"))?;
    fs::create_dir_all(dir)
        .with_context(|| format!("creating Pi extension directory {}", dir.display()))?;
    let tmp = dir.join(format!("jjfx-lifecycle.{}.tmp", std::process::id()));
    fs::write(&tmp, pi_extension_source())
        .with_context(|| format!("writing temporary Pi extension {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("installing Pi extension {}", path.display()))?;
    Ok(())
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
    reports.push(pi_extension_status(&paths.pi_extension)?);
    Ok(reports)
}

fn pi_extension_status(path: &Path) -> anyhow::Result<StatusReport> {
    let mut report = StatusReport {
        agent: "pi",
        installed: Vec::new(),
        missing: Vec::new(),
        outdated: Vec::new(),
        conflicting: Vec::new(),
    };
    match fs::read_to_string(path) {
        Ok(source) if source == pi_extension_source() => {
            report.installed.push(PI_EXTENSION_NAME.to_owned());
        }
        Ok(source) if source.contains(PI_EXTENSION_MARKER) => {
            report.outdated.push(PI_EXTENSION_NAME.to_owned());
        }
        Ok(_) => report.conflicting.push(PI_EXTENSION_NAME.to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            report.missing.push(PI_EXTENSION_NAME.to_owned());
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
    fn pi_extension_emits_the_versioned_lifecycle_contract() {
        let source = pi_extension_source();
        for (pi_event, lifecycle_event) in [
            ("session_start", "SessionStart"),
            ("agent_start", "UserPromptSubmit"),
            ("turn_start", "UserPromptSubmit"),
            ("agent_settled", "Stop"),
            ("session_shutdown", "SessionEnd"),
        ] {
            assert!(source.contains(pi_event), "missing Pi event {pi_event}");
            assert!(
                source.contains(lifecycle_event),
                "missing normalized event {lifecycle_event}"
            );
        }
        for field in [
            "jjfx_event_version",
            "hook_event_name",
            "agent_kind",
            "session_id",
            "cwd",
        ] {
            assert!(source.contains(field), "missing envelope field {field}");
        }
        assert!(source.contains("appendFile"));
        assert!(!source.contains("PermissionRequest"));
    }

    #[test]
    fn pi_extension_install_is_idempotent_and_preserves_other_state() {
        let dir = tempfile::tempdir().unwrap();
        let pi_dir = dir.path().join("pi-agent");
        let paths = IntegrationPaths {
            claude_settings: dir.path().join("claude/settings.json"),
            codex_hooks: dir.path().join("codex/hooks.json"),
            pi_extension: pi_dir.join("extensions/jjfx-lifecycle.ts"),
        };
        fs::create_dir_all(pi_dir.join("extensions")).unwrap();
        fs::write(pi_dir.join("settings.json"), "{\"theme\":\"custom\"}\n").unwrap();
        fs::write(pi_dir.join("trust.json"), "{\"/project\":\"yes\"}\n").unwrap();
        fs::write(
            pi_dir.join("extensions/other.ts"),
            "export default () => {};\n",
        )
        .unwrap();

        let first = install_with_paths(&paths).unwrap();
        let pi = first.iter().find(|outcome| outcome.agent == "pi").unwrap();
        assert_eq!(pi.added, ["lifecycle extension"]);
        assert!(pi.already.is_empty());
        assert_eq!(
            fs::read_to_string(&paths.pi_extension).unwrap(),
            pi_extension_source()
        );
        assert_eq!(
            fs::read_to_string(pi_dir.join("settings.json")).unwrap(),
            "{\"theme\":\"custom\"}\n"
        );
        assert_eq!(
            fs::read_to_string(pi_dir.join("trust.json")).unwrap(),
            "{\"/project\":\"yes\"}\n"
        );
        assert_eq!(
            fs::read_to_string(pi_dir.join("extensions/other.ts")).unwrap(),
            "export default () => {};\n"
        );

        let second = install_with_paths(&paths).unwrap();
        let pi = second.iter().find(|outcome| outcome.agent == "pi").unwrap();
        assert!(pi.added.is_empty());
        assert_eq!(pi.already, ["lifecycle extension"]);
    }

    #[test]
    fn pi_extension_status_distinguishes_missing_outdated_and_conflicting_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("extensions/jjfx-lifecycle.ts");

        let missing = pi_extension_status(&path).unwrap();
        assert_eq!(missing.missing, [PI_EXTENSION_NAME]);

        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "// jjfx-pi-lifecycle-extension:v0\n").unwrap();
        let outdated = pi_extension_status(&path).unwrap();
        assert_eq!(outdated.outdated, [PI_EXTENSION_NAME]);
        let updated = install_pi_extension(&path).unwrap();
        assert_eq!(updated.updated, [PI_EXTENSION_NAME]);
        assert_eq!(fs::read_to_string(&path).unwrap(), pi_extension_source());

        fs::write(&path, "export default () => {};\n").unwrap();
        let conflicting = pi_extension_status(&path).unwrap();
        assert_eq!(conflicting.conflicting, [PI_EXTENSION_NAME]);
        let error = install_pi_extension(&path).unwrap_err();
        assert!(error.to_string().contains("refusing to overwrite"));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "export default () => {};\n"
        );
    }

    #[test]
    fn installed_pi_envelopes_replay_through_the_shared_log_contract() {
        use crate::agent::{AgentKind, AgentState, AgentStates};

        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let paths = IntegrationPaths {
            claude_settings: dir.path().join("claude/settings.json"),
            codex_hooks: dir.path().join("codex/hooks.json"),
            pi_extension: dir.path().join("pi-agent/extensions/jjfx-lifecycle.ts"),
        };
        install_with_paths(&paths).unwrap();

        let log = dir.path().join("state/jjfx/events.jsonl");
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        let lines = [
            json!({
                "jjfx_event_version": 1,
                "hook_event_name": "SessionStart",
                "agent_kind": "pi",
                "session_id": "pi-integration",
                "cwd": workspace,
            }),
            json!({
                "jjfx_event_version": 1,
                "hook_event_name": "UserPromptSubmit",
                "agent_kind": "pi",
                "session_id": "pi-integration",
                "cwd": workspace,
            }),
            json!({
                "jjfx_event_version": 1,
                "hook_event_name": "Stop",
                "agent_kind": "pi",
                "session_id": "pi-integration",
                "cwd": workspace,
            }),
        ];
        let text = lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(&log, text).unwrap();

        let states = AgentStates::replay(crate::events::read_events(&log));
        let agent = states.agent_for(&workspace);
        assert_eq!(agent.kind, AgentKind::Pi);
        assert_eq!(agent.state, AgentState::Waiting);
    }

    #[test]
    fn installed_extension_observes_an_isolated_real_pi_when_available() {
        use std::process::{Command, Stdio};
        use std::thread;
        use std::time::{Duration, Instant};

        use crate::agent::{AgentKind, AgentState, AgentStates};

        let Ok(probe) = Command::new("pi").arg("--version").output() else {
            return;
        };
        if !probe.status.success()
            || !String::from_utf8_lossy(&probe.stdout)
                .trim()
                .starts_with("0.84.")
        {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let state = dir.path().join("state");
        let pi_dir = dir.path().join("pi-agent");
        let workspace = dir.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let paths = IntegrationPaths {
            claude_settings: home.join(".claude/settings.json"),
            codex_hooks: home.join(".codex/hooks.json"),
            pi_extension: pi_dir.join("extensions/jjfx-lifecycle.ts"),
        };
        install_with_paths(&paths).unwrap();

        let mut child = Command::new("pi")
            .args(["--mode", "rpc", "--no-session", "--offline"])
            .current_dir(&workspace)
            .env("HOME", &home)
            .env("XDG_STATE_HOME", &state)
            .env("PI_CODING_AGENT_DIR", &pi_dir)
            .env("PI_OFFLINE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("isolated Pi lifecycle smoke test timed out");
            }
            thread::sleep(Duration::from_millis(50));
        };
        assert!(status.success(), "isolated Pi exited with {status}");

        let log = state.join("jjfx/events.jsonl");
        let states = AgentStates::replay(crate::events::read_events(&log));
        let agent = states.agent_for(&workspace);
        assert_eq!(agent.kind, AgentKind::Pi);
        assert_eq!(agent.state, AgentState::Ended);
    }

    #[test]
    fn conflicting_pi_extension_fails_before_other_integrations_are_written() {
        let dir = tempfile::tempdir().unwrap();
        let paths = IntegrationPaths {
            claude_settings: dir.path().join("claude/settings.json"),
            codex_hooks: dir.path().join("codex/hooks.json"),
            pi_extension: dir.path().join("pi-agent/extensions/jjfx-lifecycle.ts"),
        };
        fs::create_dir_all(paths.pi_extension.parent().unwrap()).unwrap();
        fs::write(&paths.pi_extension, "export default () => {};\n").unwrap();

        let error = install_with_paths(&paths).unwrap_err();
        assert!(error.to_string().contains("refusing to overwrite"));
        assert!(!paths.claude_settings.exists());
        assert!(!paths.codex_hooks.exists());
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
