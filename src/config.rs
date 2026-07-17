//! jjfx's own settings, read once at startup from a TOML file. Distinct from jj
//! config (read via the `jj` CLI) and the event log - this is the only file
//! jjfx itself owns. Absent file -> defaults; malformed file -> a startup error
//! surfaced before the TUI takes over the screen.

use std::fs;
use std::path::PathBuf;

use anyhow::Context;
use serde::Deserialize;

/// The whole jjfx config tree.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// How jjfx launches the coding agent in each workspace.
    pub agent: AgentConfig,
    /// Which terminal instance hosts workspace sessions, and how to reach it.
    pub terminal: TerminalConfig,
    /// How the forge pipeline opens and maintains pull requests.
    pub forge: ForgeConfig,
}

impl Config {
    /// The command run in a workspace's left pane, wrapped in the user's login
    /// interactive shell as `$SHELL -l -i -c <command>`. The shell sources
    /// login and interactive files, so configured aliases and the user's PATH
    /// are available. Falls back to `/bin/sh` when `$SHELL` is unset.
    pub fn agent_command(&self) -> Vec<String> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        vec![
            shell,
            "-l".to_string(),
            "-i".to_string(),
            "-c".to_string(),
            self.agent.command.clone(),
        ]
    }
}

/// How jjfx launches the coding agent in a workspace.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    /// The shell command run in a workspace's left pane. Because it runs in the
    /// user's login interactive shell, this may name an alias such as `cx`.
    pub command: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            command: "claude".to_string(),
        }
    }
}

/// How the forge's final step manages pull requests. jjfx submits PRs natively
/// over `gh` (ADR 0007) - no third-party CLI - and these settings gate it.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ForgeConfig {
    /// Whether the forge creates/updates PRs at all. On by default; set false to
    /// stop the pipeline after push and open PRs yourself.
    pub pull_requests: bool,
    /// Open newly-created PRs as drafts. On by default; set false to open them
    /// ready for review.
    pub draft: bool,
}

impl Default for ForgeConfig {
    fn default() -> Self {
        // Both default on: forging a workspace opens a draft PR out of the box.
        ForgeConfig {
            pull_requests: true,
            draft: true,
        }
    }
}

/// Where jjfx opens workspace session tabs. Empty (the default) drives the kitty
/// jjfx runs inside, matching pre-config behaviour.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalConfig {
    /// The target kitty's `listen_on` *base* value, e.g. `unix:/tmp/kitty-visor`
    /// (exactly what you pass kitty, not the live socket). kitty appends `-<pid>`
    /// to a unix path, so jjfx resolves the actual `/tmp/kitty-visor-<pid>` at
    /// call time and routes every `kitten @` there via `--to`. `None` uses the
    /// inherited `KITTY_LISTEN_ON` (the kitty jjfx itself runs in).
    pub listen_on: Option<String>,
    /// Command jjfx runs to launch the target when its socket isn't found - the
    /// program followed by its arguments, e.g.
    /// `["/Applications/Visor.app/Contents/MacOS/kitty", "--detach", "-o",
    /// "allow_remote_control=yes", "-o", "listen_on=unix:/tmp/kitty-visor"]`.
    /// It should detach (return promptly) - jjfx then polls `listen_on` until
    /// the instance answers. Empty (the default) never auto-launches; jjfx just
    /// reports the target as not running.
    #[serde(default)]
    pub launch_command: Vec<String>,
}

/// `${XDG_CONFIG_HOME:-~/.config}/jjfx/config.toml` - the same XDG convention as
/// the event log's state path.
pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            home.join(".config")
        });
    base.join("jjfx").join("config.toml")
}

/// Load the config from its fixed path. A missing file yields defaults; a file
/// that exists but fails to parse is an error (named so the user can find it).
pub fn load() -> anyhow::Result<Config> {
    load_from(&config_path())
}

/// The parse step, split out so tests can drive it from a fixture path.
fn load_from(path: &std::path::Path) -> anyhow::Result<Config> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(e).with_context(|| format!("reading config {}", path.display())),
    };
    toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_is_all_defaults() {
        let cfg: Config = toml::from_str("").expect("empty toml parses");
        assert!(cfg.terminal.listen_on.is_none());
        assert!(cfg.terminal.launch_command.is_empty());
        assert_eq!(cfg.agent.command, "claude");
        // Forge PR management is on-by-default, drafts on-by-default.
        assert!(cfg.forge.pull_requests);
        assert!(cfg.forge.draft);
    }

    #[test]
    fn forge_section_overrides_only_the_keys_given() {
        let cfg: Config = toml::from_str(
            r#"
            [forge]
            draft = false
            "#,
        )
        .expect("toml parses");
        // Unspecified key keeps its default; the given one is overridden.
        assert!(cfg.forge.pull_requests);
        assert!(!cfg.forge.draft);
    }

    #[test]
    fn forge_can_disable_pull_requests() {
        let cfg: Config = toml::from_str("[forge]\npull_requests = false\n").expect("toml parses");
        assert!(!cfg.forge.pull_requests);
        assert!(cfg.forge.draft);
    }

    #[test]
    fn full_terminal_section_parses() {
        let cfg: Config = toml::from_str(
            r#"
            [terminal]
            listen_on = "unix:/tmp/kitty-visor"
            launch_command = ["kitty", "--detach", "-o", "listen_on=unix:/tmp/kitty-visor"]
            "#,
        )
        .expect("toml parses");
        assert_eq!(
            cfg.terminal.listen_on.as_deref(),
            Some("unix:/tmp/kitty-visor")
        );
        assert_eq!(
            cfg.terminal.launch_command,
            ["kitty", "--detach", "-o", "listen_on=unix:/tmp/kitty-visor"]
        );
    }

    #[test]
    fn the_removed_agent_selector_is_rejected() {
        let err = toml::from_str::<Config>("[terminal]\nagent = \"codex\"\n")
            .expect_err("removed agent selector is an error");
        assert!(err.to_string().contains("agent"), "{err}");
    }

    #[test]
    fn a_configured_agent_command_runs_through_the_login_shell() {
        let cfg: Config = toml::from_str(
            r#"
            [agent]
            command = "cx"
            "#,
        )
        .expect("toml parses");
        let cmd = cfg.agent_command();
        assert_eq!(&cmd[1..], ["-l", "-i", "-c", "cx"]);
        assert!(!cmd[0].is_empty(), "a shell program is always chosen");
    }

    #[test]
    fn the_default_agent_command_gets_the_login_shell_wrap() {
        let cfg: Config = toml::from_str("").expect("empty toml parses");
        let cmd = cfg.agent_command();
        assert_eq!(&cmd[1..], ["-l", "-i", "-c", "claude"]);
        assert!(!cmd[0].is_empty(), "a shell program is always chosen");
    }

    #[test]
    fn the_removed_tool_specific_sections_are_rejected() {
        for section in ["claude", "codex"] {
            let text = format!("[{section}]\ncommand = [\"{section}\"]\n");
            let err = toml::from_str::<Config>(&text).expect_err("removed section is an error");
            assert!(err.to_string().contains(section), "{err}");
        }
    }

    #[test]
    fn partial_terminal_section_leaves_the_rest_default() {
        let cfg: Config = toml::from_str(
            r#"
            [terminal]
            listen_on = "unix:/tmp/kitty-visor"
            "#,
        )
        .expect("toml parses");
        assert_eq!(
            cfg.terminal.listen_on.as_deref(),
            Some("unix:/tmp/kitty-visor")
        );
        assert!(cfg.terminal.launch_command.is_empty());
    }

    #[test]
    fn unknown_key_is_rejected() {
        let err = toml::from_str::<Config>(
            r#"
            [terminal]
            listne_on = "typo"
            "#,
        )
        .expect_err("unknown key is an error");
        assert!(err.to_string().contains("listne_on"), "{err}");
    }

    #[test]
    fn a_missing_file_is_defaults_not_an_error() {
        let cfg = load_from(std::path::Path::new(
            "/nonexistent/jjfx/does-not-exist.toml",
        ))
        .expect("missing file yields defaults");
        assert!(cfg.terminal.listen_on.is_none());
    }
}
