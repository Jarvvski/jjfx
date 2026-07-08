//! Minimal jj reads via the CLI. The skeleton only needs the set of workspace
//! *names* jj knows about; jj does not record each workspace's filesystem path
//! (spike 02), so paths come from the ws-cache. Richer reads move to jj-lib in
//! later epics (ADR 0007).

use std::path::Path;
use std::process::Command;

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
