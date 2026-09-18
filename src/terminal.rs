//! The multiplexer seam (ticket 07): every terminal-tab operation goes through
//! the [`Terminal`] trait, so kitty is swappable later (v1 is kitty-only). No
//! `kitten @` call lives outside [`KittyTerminal`].
//!
//! A workspace's tab is identified by a `jjfx:<name>` title, keeping jjfx tabs
//! distinct from the user's other tabs and from the coexisting bash tools.
//!
//! By default jjfx drives the kitty it runs inside (kitty exports
//! `KITTY_LISTEN_ON`). Point [`TerminalConfig::listen_on`](crate::config::TerminalConfig)
//! at a different instance's socket and jjfx routes every call there instead
//! with `kitten @ --to <socket>`, launching that instance via a configured
//! `launch_command` when it isn't yet reachable.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, anyhow};
use serde_json::Value;
use wsg_core::{PaneLayout, PaneRole, SplitLocation};

use crate::cmd::cmd;
use crate::config::TerminalConfig;
use crate::display;
use crate::layout::LayoutSettings;

/// Tab-title prefix that marks (and locates) a workspace's tab.
const TAB_PREFIX: &str = "jjfx:";

fn tab_title(name: &str) -> String {
    format!("{TAB_PREFIX}{name}")
}

/// The kitty user variable every jjfx pane is tagged with, so the pane running
/// the agent can be found again when a workspace is re-opened.
const ROLE_VAR: &str = "jjfx_role";

fn role_var_args(role: PaneRole) -> [String; 2] {
    ["--var".to_string(), format!("{ROLE_VAR}={}", role.as_str())]
}

fn new_shell_tab_args(
    title: &str,
    cwd_arg: &str,
    focus: bool,
    role: PaneRole,
    command: &[String],
) -> Vec<String> {
    let mut args = vec![
        "launch".to_string(),
        "--type=tab".to_string(),
        "--tab-title".to_string(),
        title.to_string(),
        cwd_arg.to_string(),
    ];
    args.extend(role_var_args(role));
    if !focus {
        args.push("--dont-take-focus".to_string());
    }
    args.extend_from_slice(command);
    args
}

fn splits_layout_args(window_id: &str) -> Vec<String> {
    vec![
        "goto-layout".to_string(),
        "--match".to_string(),
        format!("window_id:{window_id}"),
        "splits".to_string(),
    ]
}

fn split_args(
    anchor_id: &str,
    location: &str,
    bias: u8,
    cwd_arg: &str,
    focus: bool,
    role: PaneRole,
    command: &[String],
) -> Vec<String> {
    let mut args = vec![
        "launch".to_string(),
        "--match".to_string(),
        format!("window_id:{anchor_id}"),
        "--next-to".to_string(),
        format!("id:{anchor_id}"),
        format!("--location={location}"),
        format!("--bias={bias}"),
        cwd_arg.to_string(),
    ];
    args.extend(role_var_args(role));
    if !focus {
        args.push("--dont-take-focus".to_string());
    }
    args.extend_from_slice(command);
    args
}

fn focus_window_args(window_id: &str) -> Vec<String> {
    vec![
        "focus-window".to_string(),
        "--match".to_string(),
        format!("id:{window_id}"),
    ]
}

/// One step in building a tab. Pane references are layout indices; the executor
/// replaces them with the live kitty window ids as panes open.
#[derive(Debug, Clone, PartialEq, Eq)]
enum OpenStep {
    /// Launch the tab's own pane (pane 0 of the layout).
    LaunchTab {
        role: PaneRole,
        command: Vec<String>,
    },
    /// Force kitty's splits layout, required before a biased split.
    ForceSplits,
    /// Split the pane at `anchor`; a missing `location` (only possible in an
    /// unvalidated plan) records a pane that never opened.
    Split {
        anchor: usize,
        role: PaneRole,
        location: Option<SplitLocation>,
        bias: u8,
        command: Vec<String>,
    },
    /// Focus the pane at `pane` after the tab is built.
    Focus { pane: usize },
}

/// Map a layout to the ordered steps that build it. Split out from
/// [`KittyTerminal::open`] so the layout-to-invocation mapping is unit-testable
/// without a running kitty.
fn open_steps(layout: &PaneLayout, command_for: impl Fn(PaneRole) -> Vec<String>) -> Vec<OpenStep> {
    let mut steps = Vec::with_capacity(layout.panes.len() + 2);
    for (index, pane) in layout.panes.iter().enumerate() {
        let command = command_for(pane.role);
        if index == 0 {
            steps.push(OpenStep::LaunchTab {
                role: pane.role,
                command,
            });
            if layout.panes.len() > 1 {
                // `--bias` is only a percentage under the splits layout.
                steps.push(OpenStep::ForceSplits);
            }
            continue;
        }
        steps.push(OpenStep::Split {
            anchor: pane.from,
            role: pane.role,
            location: pane.location,
            bias: pane.bias,
            command,
        });
    }
    steps.push(OpenStep::Focus {
        pane: layout.focus_pane(),
    });
    steps
}

/// A terminal multiplexer jjfx drives to host workspace tabs.
pub trait Terminal: Send + Sync {
    /// Is a tab for this workspace currently open?
    fn is_open(&self, name: &str) -> bool;
    /// Open a tab for the workspace rooted at `path` using the layout selected
    /// for the current display. `focus` lands on the layout's focus pane (the
    /// agent when it has one); otherwise the tab is built without the target
    /// taking focus.
    fn open(&self, name: &str, path: &Path, focus: bool) -> anyhow::Result<()>;
    /// Focus the workspace's existing tab.
    fn focus(&self, name: &str) -> anyhow::Result<()>;
    /// Close the workspace's tab if it exists (a no-op if it does not).
    fn close(&self, name: &str) -> anyhow::Result<()>;
}

/// Drives kitty via its remote-control protocol (`kitten @`). Requires kitty's
/// remote control to be enabled (jjfx runs inside kitty, where `KITTY_LISTEN_ON`
/// is set).
pub struct KittyTerminal {
    /// The target instance's `listen_on` base (e.g. `unix:/tmp/kitty-visor`);
    /// [`resolve_socket`] turns it into the live pid-suffixed socket for
    /// `kitten @ --to`. `None` drives the inherited `KITTY_LISTEN_ON` (the kitty
    /// jjfx runs inside).
    listen_on: Option<String>,
    /// Command (program + args) run to launch the target when its socket isn't
    /// found. Empty never auto-launches.
    launch_command: Vec<String>,
    /// Command (program + args) run in a tab's agent pane - the configured agent,
    /// already resolved by [`Config::agent_command`](crate::config::Config::agent_command)
    /// through the login interactive shell. This module just runs what it is
    /// given.
    agent_command: Vec<String>,
    /// Command (program + args) run in a tab's first (top-left) pane - the
    /// configured command, already resolved by
    /// [`Config::first_pane_command`](crate::config::Config::first_pane_command)
    /// through the login interactive shell. Empty leaves that pane a plain
    /// shell.
    first_pane_command: Vec<String>,
    /// The named layouts and display mapping from config, plus the
    /// `JJFX_LAYOUT` override. Resolved per open so a display change is picked
    /// up without restarting.
    layout: LayoutSettings,
}

/// How long to wait for a freshly-launched target to expose its socket, and how
/// often to re-probe. 25 x 200ms = ~5s, generous for a cold kitty start without
/// hanging the UI indefinitely.
const LAUNCH_PROBE_ATTEMPTS: u32 = 25;
const LAUNCH_PROBE_INTERVAL: Duration = Duration::from_millis(200);

impl KittyTerminal {
    /// Build from config plus the resolved agent and first-pane commands. Empty
    /// config reproduces the pre-config behaviour of driving the surrounding
    /// kitty.
    pub fn new(
        cfg: &TerminalConfig,
        agent_command: Vec<String>,
        first_pane_command: Vec<String>,
    ) -> Self {
        Self {
            listen_on: cfg.listen_on.clone(),
            launch_command: cfg.launch_command.clone(),
            agent_command,
            first_pane_command,
            layout: LayoutSettings::from_config(cfg),
        }
    }

    /// Run `kitten @ <args>`, returning stdout on success. Errors carry stderr so
    /// the app can surface a useful message rather than failing silently.
    fn run<S: AsRef<str>>(&self, args: &[S]) -> anyhow::Result<String> {
        let to = self.resolved_to()?;
        cmd("kitten")
            .args(kitten_argv(to.as_deref(), args))
            .run()
            .context("running `kitten @` - is kitty's remote control enabled?")?
            .checked()
    }

    /// The `--to` address for the current call: `None` (drive the surrounding
    /// kitty) when no target is configured, else the live pid-suffixed socket
    /// resolved from the configured base. An error when a target is configured
    /// but no matching socket exists yet - the target is not running.
    fn resolved_to(&self) -> anyhow::Result<Option<String>> {
        match self.listen_on.as_deref() {
            None => Ok(None),
            Some(base) => resolve_socket(base)
                .ok_or_else(|| {
                    anyhow!("no live socket matches `{base}` - is the target terminal running?")
                })
                .map(Some),
        }
    }

    /// Ensure the configured target is reachable before an operation that needs
    /// it, launching it via `launch_command` when it isn't. A no-op when no
    /// explicit target is set (the surrounding kitty is by definition already
    /// up) and when the target already answers. With no `launch_command`, a
    /// down target is left for the caller's own `kitten @` call to report.
    fn ensure_ready(&self) -> anyhow::Result<()> {
        // Without an explicit target we drive the surrounding kitty - nothing to
        // launch or wait for.
        let Some(listen_on) = self.listen_on.as_deref() else {
            return Ok(());
        };
        if self.ls().is_ok() {
            return Ok(());
        }
        // Not reachable. Launch it if a command is configured; otherwise let the
        // caller's own `kitten @` call surface the "not running" error.
        let Some((program, args)) = self.launch_command.split_first() else {
            return Ok(());
        };
        cmd(program)
            .args(args)
            .spawn_detached()
            .context("running the terminal `launch_command`")?;

        // Wait for the launched instance to expose its remote-control socket.
        for _ in 0..LAUNCH_PROBE_ATTEMPTS {
            std::thread::sleep(LAUNCH_PROBE_INTERVAL);
            if self.ls().is_ok() {
                return Ok(());
            }
        }
        anyhow::bail!(
            "target terminal did not answer at {listen_on} after running `{program}` - \
             does the command enable remote control on that socket?"
        )
    }

    /// Parse `kitten @ ls` into its JSON tree.
    fn ls(&self) -> anyhow::Result<Value> {
        let json = self.run(&["ls"])?;
        serde_json::from_str(&json).context("parsing `kitten @ ls` output")
    }

    /// `launch` a window and return the new window's id (kitty prints it to
    /// stdout), trimmed. The id anchors later splits with `--match id:<id>`.
    fn launch<S: AsRef<str>>(&self, args: &[S]) -> anyhow::Result<String> {
        Ok(self.run(args)?.trim().to_string())
    }

    /// The command for a pane role: the configured first-pane command, the
    /// agent command, or nothing (kitty's default shell).
    fn pane_command(&self, role: PaneRole) -> &[String] {
        match role {
            PaneRole::First => &self.first_pane_command,
            PaneRole::Agent => &self.agent_command,
            PaneRole::Shell => &[],
        }
    }

    /// The window id of the agent pane in `name`'s tab, when the tab has one.
    /// Tagged panes are found by role; tabs opened before panes were tagged
    /// fall back to matching the configured agent command.
    fn agent_window(&self, name: &str) -> Option<String> {
        let tree = self.ls().ok()?;
        let title = tab_title(name);
        tagged_window(&tree, &title, PaneRole::Agent)
            .or_else(|| window_with_command(&tree, &title, &self.agent_command))
            .map(|id| id.to_string())
    }
}

impl Terminal for KittyTerminal {
    fn is_open(&self, name: &str) -> bool {
        let title = tab_title(name);
        let Ok(tree) = self.ls() else {
            return false;
        };
        tree.as_array()
            .into_iter()
            .flatten()
            .filter_map(|osw| osw.get("tabs").and_then(Value::as_array))
            .flatten()
            .any(|tab| tab.get("title").and_then(Value::as_str) == Some(title.as_str()))
    }

    fn open(&self, name: &str, path: &Path, focus: bool) -> anyhow::Result<()> {
        // A tab can only be created once the target instance answers; launch it
        // first if it is configured but not yet running.
        self.ensure_ready()?;

        let layout = self.layout.resolve(display::attached_condition())?;
        let title = tab_title(name);
        let cwd = path.to_string_lossy();
        let cwd_arg = format!("--cwd={cwd}");

        // Window ids stay aligned with layout pane indices: a pane that fails
        // to open records `None`, and panes anchored to it are skipped.
        let mut windows: Vec<Option<String>> = Vec::with_capacity(layout.panes.len());
        for step in open_steps(layout, |role| self.pane_command(role).to_vec()) {
            match step {
                OpenStep::LaunchTab { role, command } => {
                    let id =
                        self.launch(&new_shell_tab_args(&title, &cwd_arg, focus, role, &command))?;
                    if id.is_empty() {
                        // The tab exists but has no id to anchor splits to.
                        return Ok(());
                    }
                    windows.push(Some(id));
                }
                OpenStep::ForceSplits => {
                    if let Some(Some(tab)) = windows.first() {
                        self.run(&splits_layout_args(tab))?;
                    }
                }
                OpenStep::Split {
                    anchor,
                    role,
                    location,
                    bias,
                    command,
                } => {
                    let anchor = windows.get(anchor).and_then(Option::as_deref);
                    let Some((anchor, location)) = anchor.zip(location) else {
                        windows.push(None);
                        continue;
                    };
                    let id = self.launch(&split_args(
                        anchor,
                        location.as_kitty_value(),
                        bias,
                        &cwd_arg,
                        focus,
                        role,
                        &command,
                    ))?;
                    windows.push(if id.is_empty() { None } else { Some(id) });
                }
                OpenStep::Focus { pane } => {
                    if !focus {
                        continue;
                    }
                    if let Some(id) = windows.get(pane).and_then(Option::as_deref) {
                        self.run(&focus_window_args(id))?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Focus the workspace's tab and land keyboard focus in its agent pane.
    /// Switching to a tab restores whatever pane was last active there, which
    /// is rarely the agent when returning to a workspace, so jjfx re-focuses
    /// the tagged agent window. A tab without one (a custom layout, or one
    /// built before the agent existed) keeps its own focus.
    fn focus(&self, name: &str) -> anyhow::Result<()> {
        self.run(&["focus-tab", "--match", &title_match(name)])?;
        if let Some(agent) = self.agent_window(name) {
            let _ = self.run(&focus_window_args(&agent));
        }
        Ok(())
    }

    fn close(&self, name: &str) -> anyhow::Result<()> {
        if !self.is_open(name) {
            return Ok(());
        }
        self.run(&["close-tab", "--match", &title_match(name)])?;
        Ok(())
    }
}

/// Build the argv after the `kitten` program name: always the `@` verb, an
/// optional `--to <socket>` to target a specific instance, then the subcommand
/// args. Split out from [`KittyTerminal::run`] so the routing is unit-testable
/// without spawning kitty.
fn kitten_argv<S: AsRef<str>>(listen_on: Option<&str>, args: &[S]) -> Vec<String> {
    let mut argv = Vec::with_capacity(args.len() + 3);
    argv.push("@".to_string());
    if let Some(socket) = listen_on {
        argv.push("--to".to_string());
        argv.push(socket.to_string());
    }
    argv.extend(args.iter().map(|s| s.as_ref().to_string()));
    argv
}

/// Resolve a configured base address to the socket kitty actually created.
/// kitty appends `-<pid>` to a unix socket path, so a base of
/// `unix:/tmp/kitty-visor` materialises as `/tmp/kitty-visor-48040` and changes
/// every launch - jjfx must discover the live one. Non-unix addresses (`tcp:`)
/// and Linux abstract sockets (`unix:@name`) carry no pid suffix and pass
/// through verbatim. Returns `None` when no matching socket exists (target down).
fn resolve_socket(base: &str) -> Option<String> {
    let Some(path) = base.strip_prefix("unix:") else {
        return Some(base.to_string()); // tcp: (or anything non-unix) - used as-is
    };
    if path.starts_with('@') {
        return Some(base.to_string()); // abstract socket (Linux) - no pid suffix
    }
    let path = Path::new(path);
    // A base pinned to an exact, existing socket wins (e.g. the user embedded
    // {kitty_pid} themselves, so kitty did not append its own suffix).
    if path.exists() {
        return Some(base.to_string());
    }
    // Otherwise find `<base_name>-<pid>`, most recently created first (the tie-
    // break when more than one instance of the target is somehow running).
    let dir = path.parent()?;
    let base_name = path.file_name()?.to_str()?;
    let newest = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            if !is_pid_socket(base_name, name.to_str()?) {
                return None;
            }
            let mtime = entry.metadata().ok()?.modified().ok()?;
            Some((mtime, entry.path()))
        })
        .max_by_key(|(mtime, _)| *mtime)?;
    Some(format!("unix:{}", newest.1.to_string_lossy()))
}

/// Does `name` look like kitty's pid-suffixed socket for base `base_name`, i.e.
/// exactly `<base_name>-<digits>`? The all-digits tail keeps `kitty` from
/// matching `kitty-visor-48040` (its tail `visor-48040` is not all digits).
fn is_pid_socket(base_name: &str, name: &str) -> bool {
    match name
        .strip_prefix(base_name)
        .and_then(|r| r.strip_prefix('-'))
    {
        Some(pid) => !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

/// The tab titled `title` in a `kitten @ ls` tree, if it exists.
fn tab_in<'a>(tree: &'a Value, title: &str) -> Option<&'a Value> {
    tree.as_array()?
        .iter()
        .filter_map(|os_window| os_window.get("tabs")?.as_array())
        .flatten()
        .find(|tab| tab.get("title").and_then(Value::as_str) == Some(title))
}

/// The id of the window in the tab titled `title` that carries `role`'s jjfx
/// tag, if any. Panes are tagged at launch ([`role_var_args`]), so a role is
/// found by what jjfx put there rather than by a live process name.
fn tagged_window(tree: &Value, title: &str, role: PaneRole) -> Option<i64> {
    tab_in(tree, title)?
        .get("windows")?
        .as_array()?
        .iter()
        .find(|window| {
            window
                .get("user_vars")
                .and_then(|vars| vars.get(ROLE_VAR))
                .and_then(Value::as_str)
                == Some(role.as_str())
        })?
        .get("id")?
        .as_i64()
}

/// The id of the window in the tab titled `title` whose launch command is
/// `command`, if any. This finds the agent pane in tabs built before panes were
/// tagged with roles.
fn window_with_command(tree: &Value, title: &str, command: &[String]) -> Option<i64> {
    tab_in(tree, title)?
        .get("windows")?
        .as_array()?
        .iter()
        .find(|window| {
            window
                .get("cmdline")
                .and_then(Value::as_array)
                .is_some_and(|cmdline| {
                    cmdline.len() == command.len()
                        && cmdline
                            .iter()
                            .zip(command)
                            .all(|(actual, expected)| actual.as_str() == Some(expected.as_str()))
                })
        })?
        .get("id")?
        .as_i64()
}

/// An anchored title match for kitty's `--match`, so `feat` never matches
/// `feature`. kitty matches `title:` as a regex.
fn title_match(name: &str) -> String {
    format!("title:^{}$", regex_escape(&tab_title(name)))
}

/// Escape the regex metacharacters that can appear in a workspace name so the
/// title match stays literal.
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.^$|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wsg_core::PanePlacement;

    #[test]
    fn tab_title_is_prefixed() {
        assert_eq!(tab_title("feat"), "jjfx:feat");
    }

    #[test]
    fn kitten_argv_without_target_is_unchanged() {
        // No `listen_on` -> the plain `@ <args>` jjfx has always sent, so it
        // still drives the surrounding kitty via KITTY_LISTEN_ON.
        assert_eq!(kitten_argv(None, &["ls"]), vec!["@", "ls"]);
    }

    #[test]
    fn kitten_argv_routes_to_the_configured_socket() {
        assert_eq!(
            kitten_argv(
                Some("unix:/tmp/kitty-visor"),
                &["focus-tab", "--match", "title:^x$"]
            ),
            vec![
                "@",
                "--to",
                "unix:/tmp/kitty-visor",
                "focus-tab",
                "--match",
                "title:^x$"
            ],
        );
    }

    /// The command roles used by the layout tests, so the assertions below name
    /// the roles rather than repeating the vectors.
    fn role_command(role: PaneRole) -> Vec<String> {
        match role {
            PaneRole::First => vec!["dev-server".to_string()],
            PaneRole::Agent => vec!["pi".to_string()],
            PaneRole::Shell => Vec::new(),
        }
    }

    fn pane(
        role: PaneRole,
        from: usize,
        location: Option<SplitLocation>,
        bias: u8,
    ) -> PanePlacement {
        PanePlacement {
            role,
            from,
            location,
            bias,
        }
    }

    #[test]
    fn builtin_layout_builds_the_tab_then_biased_splits_then_focus() {
        let steps = open_steps(&PaneLayout::builtin(), role_command);
        assert_eq!(
            steps,
            vec![
                OpenStep::LaunchTab {
                    role: PaneRole::First,
                    command: vec!["dev-server".to_string()],
                },
                OpenStep::ForceSplits,
                OpenStep::Split {
                    anchor: 0,
                    role: PaneRole::Agent,
                    location: Some(SplitLocation::Vsplit),
                    bias: 81,
                    command: vec!["pi".to_string()],
                },
                OpenStep::Split {
                    anchor: 0,
                    role: PaneRole::Shell,
                    location: Some(SplitLocation::Hsplit),
                    bias: 50,
                    command: Vec::new(),
                },
                OpenStep::Focus { pane: 1 },
            ]
        );
    }

    #[test]
    fn a_wider_left_column_changes_only_the_agent_bias() {
        let layout = PaneLayout {
            panes: vec![
                pane(PaneRole::First, 0, None, 50),
                pane(PaneRole::Agent, 0, Some(SplitLocation::Vsplit), 70),
                pane(PaneRole::Shell, 0, Some(SplitLocation::Hsplit), 50),
            ],
        };
        let steps = open_steps(&layout, role_command);
        assert_eq!(
            steps,
            vec![
                OpenStep::LaunchTab {
                    role: PaneRole::First,
                    command: vec!["dev-server".to_string()],
                },
                OpenStep::ForceSplits,
                OpenStep::Split {
                    anchor: 0,
                    role: PaneRole::Agent,
                    location: Some(SplitLocation::Vsplit),
                    bias: 70,
                    command: vec!["pi".to_string()],
                },
                OpenStep::Split {
                    anchor: 0,
                    role: PaneRole::Shell,
                    location: Some(SplitLocation::Hsplit),
                    bias: 50,
                    command: Vec::new(),
                },
                OpenStep::Focus { pane: 1 },
            ]
        );
    }

    #[test]
    fn a_single_pane_layout_launches_only_the_tab() {
        let layout = PaneLayout {
            panes: vec![pane(PaneRole::Agent, 0, None, 50)],
        };
        // No splits means no splits layout to force.
        assert_eq!(
            open_steps(&layout, role_command),
            vec![
                OpenStep::LaunchTab {
                    role: PaneRole::Agent,
                    command: vec!["pi".to_string()],
                },
                OpenStep::Focus { pane: 0 },
            ]
        );
    }

    #[test]
    fn split_anchors_and_focus_follow_the_plan() {
        let layout = PaneLayout {
            panes: vec![
                pane(PaneRole::First, 0, None, 50),
                pane(PaneRole::Agent, 0, Some(SplitLocation::Vsplit), 81),
                pane(PaneRole::Shell, 1, Some(SplitLocation::Hsplit), 50),
            ],
        };
        let steps = open_steps(&layout, role_command);
        assert_eq!(
            steps,
            vec![
                OpenStep::LaunchTab {
                    role: PaneRole::First,
                    command: vec!["dev-server".to_string()],
                },
                OpenStep::ForceSplits,
                OpenStep::Split {
                    anchor: 0,
                    role: PaneRole::Agent,
                    location: Some(SplitLocation::Vsplit),
                    bias: 81,
                    command: vec!["pi".to_string()],
                },
                OpenStep::Split {
                    anchor: 1,
                    role: PaneRole::Shell,
                    location: Some(SplitLocation::Hsplit),
                    bias: 50,
                    command: Vec::new(),
                },
                OpenStep::Focus { pane: 1 },
            ]
        );
    }

    #[test]
    fn a_plan_without_an_agent_focuses_its_first_pane() {
        let layout = PaneLayout {
            panes: vec![
                pane(PaneRole::First, 0, None, 50),
                pane(PaneRole::Shell, 0, Some(SplitLocation::Vsplit), 50),
            ],
        };
        assert_eq!(
            open_steps(&layout, role_command).last(),
            Some(&OpenStep::Focus { pane: 0 })
        );
    }

    #[test]
    fn workspace_layout_places_the_agent_right_of_a_split_shell_column() {
        let agent_command = vec!["zsh".to_string(), "-lc".to_string(), "pi".to_string()];

        assert_eq!(
            new_shell_tab_args("jjfx:feat", "--cwd=/repo-feat", true, PaneRole::First, &[],),
            [
                "launch",
                "--type=tab",
                "--tab-title",
                "jjfx:feat",
                "--cwd=/repo-feat",
                "--var",
                "jjfx_role=first",
            ]
        );
        // A configured first-pane command rides on the tab launch, making the
        // top-left pane run it instead of opening a plain shell.
        assert_eq!(
            new_shell_tab_args(
                "jjfx:feat",
                "--cwd=/repo-feat",
                true,
                PaneRole::First,
                &[
                    "zsh".to_string(),
                    "-lc".to_string(),
                    "dev-server".to_string()
                ],
            ),
            [
                "launch",
                "--type=tab",
                "--tab-title",
                "jjfx:feat",
                "--cwd=/repo-feat",
                "--var",
                "jjfx_role=first",
                "zsh",
                "-lc",
                "dev-server",
            ]
        );
        assert_eq!(
            splits_layout_args("41"),
            ["goto-layout", "--match", "window_id:41", "splits"]
        );
        // The agent is the new window in the split, so it carries the bias and
        // takes 81% of the width, leaving the shell column the measured 19%.
        assert_eq!(
            split_args(
                "41",
                "vsplit",
                81,
                "--cwd=/repo-feat",
                true,
                PaneRole::Agent,
                &agent_command,
            ),
            [
                "launch",
                "--match",
                "window_id:41",
                "--next-to",
                "id:41",
                "--location=vsplit",
                "--bias=81",
                "--cwd=/repo-feat",
                "--var",
                "jjfx_role=agent",
                "zsh",
                "-lc",
                "pi",
            ]
        );
        // The left column is then split into two evenly stacked shells.
        assert_eq!(
            split_args(
                "41",
                "hsplit",
                50,
                "--cwd=/repo-feat",
                true,
                PaneRole::Shell,
                &[],
            ),
            [
                "launch",
                "--match",
                "window_id:41",
                "--next-to",
                "id:41",
                "--location=hsplit",
                "--bias=50",
                "--cwd=/repo-feat",
                "--var",
                "jjfx_role=shell",
            ]
        );
        assert_eq!(
            focus_window_args("42"),
            ["focus-window", "--match", "id:42"]
        );
    }

    #[test]
    fn background_workspace_layout_keeps_focus_during_every_launch() {
        let agent_command = vec!["pi".to_string()];

        assert_eq!(
            new_shell_tab_args("jjfx:feat", "--cwd=/repo-feat", false, PaneRole::First, &[],),
            [
                "launch",
                "--type=tab",
                "--tab-title",
                "jjfx:feat",
                "--cwd=/repo-feat",
                "--var",
                "jjfx_role=first",
                "--dont-take-focus",
            ]
        );
        assert_eq!(
            split_args(
                "41",
                "vsplit",
                81,
                "--cwd=/repo-feat",
                false,
                PaneRole::Agent,
                &agent_command,
            ),
            [
                "launch",
                "--match",
                "window_id:41",
                "--next-to",
                "id:41",
                "--location=vsplit",
                "--bias=81",
                "--cwd=/repo-feat",
                "--var",
                "jjfx_role=agent",
                "--dont-take-focus",
                "pi",
            ]
        );
        assert_eq!(
            split_args(
                "41",
                "hsplit",
                50,
                "--cwd=/repo-feat",
                false,
                PaneRole::Shell,
                &[],
            ),
            [
                "launch",
                "--match",
                "window_id:41",
                "--next-to",
                "id:41",
                "--location=hsplit",
                "--bias=50",
                "--cwd=/repo-feat",
                "--var",
                "jjfx_role=shell",
                "--dont-take-focus",
            ]
        );
    }

    #[test]
    fn tagged_window_finds_the_role_in_the_matching_tab_only() {
        let tree: Value = serde_json::from_str(
            r#"
            [
              {
                "tabs": [
                  {
                    "title": "jjfx:feat",
                    "windows": [
                      { "id": 41, "user_vars": { "jjfx_role": "first" } },
                      { "id": 42, "user_vars": { "jjfx_role": "agent" } },
                      { "id": 43, "user_vars": { "jjfx_role": "shell" } }
                    ]
                  },
                  {
                    "title": "jjfx:other",
                    "windows": [
                      { "id": 51, "user_vars": { "jjfx_role": "agent" } }
                    ]
                  },
                  {
                    "title": "jjfx:legacy",
                    "windows": [
                      { "id": 61, "cmdline": ["/bin/zsh", "-l", "-i", "-c", "opencode"] },
                      { "id": 62, "cmdline": ["/bin/zsh"] }
                    ]
                  }
                ]
              }
            ]
            "#,
        )
        .expect("ls fixture parses");

        assert_eq!(tagged_window(&tree, "jjfx:feat", PaneRole::Agent), Some(42));
        assert_eq!(
            tagged_window(&tree, "jjfx:other", PaneRole::Agent),
            Some(51)
        );
        // A tab whose panes predate the tag has no match, and a missing tab is
        // not a panic.
        assert_eq!(tagged_window(&tree, "jjfx:untagged", PaneRole::Agent), None);
        assert_eq!(tagged_window(&tree, "jjfx:feat", PaneRole::First), Some(41));

        let agent_command = ["/bin/zsh", "-l", "-i", "-c", "opencode"].map(String::from);
        assert_eq!(
            window_with_command(&tree, "jjfx:legacy", &agent_command),
            Some(61)
        );
        assert_eq!(
            window_with_command(&tree, "jjfx:feat", &agent_command),
            None
        );
        assert_eq!(
            window_with_command(&tree, "jjfx:untagged", &agent_command),
            None
        );
    }

    #[test]
    fn pid_socket_matches_only_a_digit_suffix() {
        // kitty appends `-<pid>` to the configured base.
        assert!(is_pid_socket("kitty-visor", "kitty-visor-48040"));
        assert!(is_pid_socket("kitty", "kitty-19678"));
        // The unsuffixed base itself is not the live socket.
        assert!(!is_pid_socket("kitty-visor", "kitty-visor"));
        // A non-digit tail is a different instance, not a pid suffix - this is
        // what stops the `kitty` base from swallowing `kitty-visor-48040`.
        assert!(!is_pid_socket("kitty", "kitty-visor-48040"));
        assert!(!is_pid_socket("kitty-visor", "kitty-visor-"));
        assert!(!is_pid_socket("kitty-visor", "other-1"));
    }

    #[test]
    fn resolve_socket_passes_non_unix_through() {
        assert_eq!(
            resolve_socket("tcp:localhost:4000").as_deref(),
            Some("tcp:localhost:4000")
        );
        assert_eq!(
            resolve_socket("unix:@abstract").as_deref(),
            Some("unix:@abstract")
        );
    }

    #[test]
    fn title_match_is_anchored_and_escaped() {
        assert_eq!(title_match("feat"), "title:^jjfx:feat$");
        // A regex-special char in the name is escaped so the match stays literal.
        assert_eq!(title_match("a.b"), "title:^jjfx:a\\.b$");
    }
}
