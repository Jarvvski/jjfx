//! Minimal jj reads via the CLI. The skeleton only needs the set of workspace
//! *names* jj knows about; jj does not record each workspace's filesystem path
//! (spike 02), so paths come from the ws-cache. Richer reads move to jj-lib in
//! later epics (ADR 0007).

use std::path::Path;
use std::process::Command;

use anyhow::{Context, anyhow};

/// Workspace names jj knows in this repo, via
/// `jj workspace list -T 'name ++ "\n"'`. `--ignore-working-copy` keeps it a
/// pure read (no snapshot, no commit churn). Returns an empty list on any
/// failure - the skeleton degrades to the ws-cache + derived default rather than
/// erroring, and the caller cannot surface errors from inside the alt-screen.
pub fn workspace_names(repo_root: &Path) -> Vec<String> {
    let output = Command::new("jj")
        .arg("--repository")
        .arg(repo_root)
        .arg("--ignore-working-copy")
        .args(["workspace", "list", "-T", "name ++ \"\\n\""])
        .output();

    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect(),
        _ => Vec::new(),
    }
}

/// Create a new named workspace rooted at `dest` (`jj workspace add`). Unlike the
/// read paths, this is a mutation whose failure the caller surfaces, so it
/// returns a `Result` with the jj error text on failure.
pub fn add_workspace(repo_root: &Path, name: &str, dest: &Path) -> anyhow::Result<()> {
    run_mut(
        repo_root,
        &["workspace", "add", "--name", name, &dest.to_string_lossy()],
    )
}

/// Forget a workspace (`jj workspace forget`), removing jj's record of it. The
/// on-disk directory is removed separately by the caller.
pub fn forget_workspace(repo_root: &Path, name: &str) -> anyhow::Result<()> {
    run_mut(repo_root, &["workspace", "forget", name])
}

/// Run a mutating jj command, returning an error carrying jj's stderr on failure.
fn run_mut(repo_root: &Path, args: &[&str]) -> anyhow::Result<()> {
    let output = Command::new("jj")
        .arg("--repository")
        .arg(repo_root)
        .args(args)
        .output()
        .with_context(|| format!("running jj {}", args.join(" ")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "jj {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}
