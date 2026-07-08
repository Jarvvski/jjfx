//! Watch the repo's `.jj/` directory and ping the app to reload whenever it
//! changes. This catches both a shell writing `.jj/ws-cache` (the bash tools)
//! and a bare `jj workspace add` (which advances jj's op log under `.jj/`), so
//! shell-created workspaces appear in the running TUI without a restart.

use std::path::Path;

use anyhow::Context;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::Msg;

/// Start watching `<repo_root>/.jj` recursively. Every filesystem event sends a
/// single [`Msg::Reload`]; the app coalesces bursts. The returned watcher must be
/// kept alive - dropping it stops the watch.
pub fn watch_repo(
    repo_root: &Path,
    tx: UnboundedSender<Msg>,
) -> anyhow::Result<RecommendedWatcher> {
    let jj_dir = repo_root.join(".jj");
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        // Any event - create/modify/remove - is just a hint to re-reconcile.
        if res.is_ok() {
            // The receiver only closes at shutdown; ignore send errors then.
            let _ = tx.send(Msg::Reload);
        }
    })
    .context("creating filesystem watcher")?;
    watcher
        .watch(&jj_dir, RecursiveMode::Recursive)
        .with_context(|| format!("watching {}", jj_dir.display()))?;
    Ok(watcher)
}
