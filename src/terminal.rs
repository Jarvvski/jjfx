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

use crate::cmd::cmd;
use crate::config::TerminalConfig;

/// Tab-title prefix that marks (and locates) a workspace's tab.
const TAB_PREFIX: &str = "jjfx:";

fn tab_title(name: &str) -> String {
    format!("{TAB_PREFIX}{name}")
}

/// A terminal multiplexer jjfx drives to host workspace tabs.
pub trait Terminal: Send {
    /// Is a tab for this workspace currently open?
    fn is_open(&self, name: &str) -> bool;
    /// Open a tab for the workspace rooted at `path`: the agent on the left,
    /// and a right column split into two stacked shells. `focus` lands on the
    /// agent pane; otherwise the tab is built without the target taking focus.
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
    /// Command (program + args) run in a tab's left pane - the selected agent,
    /// already resolved by [`Config::agent_command`](crate::config::Config::agent_command)
    /// (config decides which agent and whether it gets the login-shell wrap;
    /// this module just runs what it is given).
    agent_command: Vec<String>,
}

/// How long to wait for a freshly-launched target to expose its socket, and how
/// often to re-probe. 25 x 200ms = ~5s, generous for a cold kitty start without
/// hanging the UI indefinitely.
const LAUNCH_PROBE_ATTEMPTS: u32 = 25;
const LAUNCH_PROBE_INTERVAL: Duration = Duration::from_millis(200);

impl KittyTerminal {
    /// Build from config plus the resolved left-pane agent command. Empty
    /// config reproduces the pre-config behaviour of driving the surrounding
    /// kitty.
    pub fn new(cfg: &TerminalConfig, agent_command: Vec<String>) -> Self {
        Self {
            listen_on: cfg.listen_on.clone(),
            launch_command: cfg.launch_command.clone(),
            agent_command,
        }
    }

    /// Run `kitten @ <args>`, returning stdout on success. Errors carry stderr so
    /// the app can surface a useful message rather than failing silently.
    fn run(&self, args: &[&str]) -> anyhow::Result<String> {
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
    fn launch(&self, args: &[&str]) -> anyhow::Result<String> {
        Ok(self.run(args)?.trim().to_string())
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

        let title = tab_title(name);
        let cwd = path.to_string_lossy();
        let cwd_arg = format!("--cwd={cwd}");
        // A background open builds the tab without pulling focus (or raising the
        // target window); a foreground open lands on the agent pane below.
        let no_focus: &[&str] = if focus { &[] } else { &["--dont-take-focus"] };

        // Left pane: a new tab running the agent. kitty prints its window id,
        // which anchors the splits so they land against the right column even
        // when this tab is not the active one.
        let mut agent = vec!["launch", "--type=tab", "--tab-title", &title, &cwd_arg];
        agent.extend_from_slice(no_focus);
        agent.extend(self.agent_command.iter().map(String::as_str));
        let win_id = self.launch(&agent)?;
        if win_id.is_empty() {
            return Ok(()); // tab exists but there is no id to anchor splits to
        }
        let win_match = format!("id:{win_id}");

        // Right column, top: a shell beside the agent (a vertical divider).
        let mut top = vec![
            "launch",
            "--match",
            &win_match,
            "--location=vsplit",
            &cwd_arg,
        ];
        top.extend_from_slice(no_focus);
        let right_top_id = self.launch(&top)?;

        // Right column, bottom: a shell below the right-top pane (a horizontal
        // divider), so the right side is split into two stacked shells.
        if !right_top_id.is_empty() {
            let top_match = format!("id:{right_top_id}");
            let mut bottom = vec![
                "launch",
                "--match",
                &top_match,
                "--location=hsplit",
                &cwd_arg,
            ];
            bottom.extend_from_slice(no_focus);
            self.run(&bottom)?;
        }

        // Land on the agent pane (kitty otherwise focuses the last-created one).
        self.run(&["focus-window", "--match", &win_match])?;
        Ok(())
    }

    fn focus(&self, name: &str) -> anyhow::Result<()> {
        self.run(&["focus-tab", "--match", &title_match(name)])?;
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
fn kitten_argv(listen_on: Option<&str>, args: &[&str]) -> Vec<String> {
    let mut argv = Vec::with_capacity(args.len() + 3);
    argv.push("@".to_string());
    if let Some(socket) = listen_on {
        argv.push("--to".to_string());
        argv.push(socket.to_string());
    }
    argv.extend(args.iter().map(|s| s.to_string()));
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
