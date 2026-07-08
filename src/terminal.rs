//! The multiplexer seam (ticket 07): every terminal-tab operation goes through
//! the [`Terminal`] trait, so kitty is swappable later (v1 is kitty-only). No
//! `kitten @` call lives outside [`KittyTerminal`].
//!
//! A workspace's tab is identified by a `jjfx:<name>` title, keeping jjfx tabs
//! distinct from the user's other tabs and from the coexisting bash tools.

use std::path::Path;

use anyhow::Context;
use serde_json::Value;

use crate::cmd::cmd;

/// Tab-title prefix that marks (and locates) a workspace's tab.
const TAB_PREFIX: &str = "jjfx:";

fn tab_title(name: &str) -> String {
    format!("{TAB_PREFIX}{name}")
}

/// A terminal multiplexer jjfx drives to host workspace tabs.
pub trait Terminal: Send {
    /// Is a tab for this workspace currently open?
    fn is_open(&self, name: &str) -> bool;
    /// Open a tab for the workspace running claude alongside a shell (a vsplit),
    /// rooted at `path`. `focus` steals focus; otherwise the tab opens in the
    /// background and the previously-focused window is restored.
    fn open(&self, name: &str, path: &Path, focus: bool) -> anyhow::Result<()>;
    /// Focus the workspace's existing tab.
    fn focus(&self, name: &str) -> anyhow::Result<()>;
    /// Close the workspace's tab if it exists (a no-op if it does not).
    fn close(&self, name: &str) -> anyhow::Result<()>;
}

/// Drives kitty via its remote-control protocol (`kitten @`). Requires kitty's
/// remote control to be enabled (jjfx runs inside kitty, where `KITTY_LISTEN_ON`
/// is set).
pub struct KittyTerminal;

impl KittyTerminal {
    /// Run `kitten @ <args>`, returning stdout on success. Errors carry stderr so
    /// the app can surface a useful message rather than failing silently.
    fn run(&self, args: &[&str]) -> anyhow::Result<String> {
        cmd("kitten")
            .arg("@")
            .args(args)
            .run()
            .context("running `kitten @` - is kitty's remote control enabled?")?
            .checked()
    }

    /// Parse `kitten @ ls` into its JSON tree.
    fn ls(&self) -> anyhow::Result<Value> {
        let json = self.run(&["ls"])?;
        serde_json::from_str(&json).context("parsing `kitten @ ls` output")
    }

    /// The id of the currently focused window, to restore focus after a
    /// background open.
    fn focused_window_id(&self) -> Option<u64> {
        let tree = self.ls().ok()?;
        for osw in tree.as_array()? {
            for tab in osw.get("tabs")?.as_array()? {
                for win in tab.get("windows")?.as_array()? {
                    if win.get("is_focused").and_then(Value::as_bool) == Some(true) {
                        return win.get("id").and_then(Value::as_u64);
                    }
                }
            }
        }
        None
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
        let title = tab_title(name);
        let cwd = path.to_string_lossy();

        // Remember focus so a background open can restore it after the new tab
        // (which kitty focuses on creation) is set up.
        let prev = if focus {
            None
        } else {
            self.focused_window_id()
        };

        // New tab running claude; a vsplit beside it running the default shell.
        self.run(&[
            "launch",
            "--type=tab",
            "--tab-title",
            &title,
            "--cwd",
            &cwd,
            "claude",
        ])?;
        self.run(&["launch", "--location=vsplit", "--cwd", &cwd])?;

        if let Some(id) = prev {
            self.run(&["focus-window", "--match", &format!("id:{id}")])?;
        }
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
    fn title_match_is_anchored_and_escaped() {
        assert_eq!(title_match("feat"), "title:^jjfx:feat$");
        // A regex-special char in the name is escaped so the match stays literal.
        assert_eq!(title_match("a.b"), "title:^jjfx:a\\.b$");
    }
}
