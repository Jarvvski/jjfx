//! The authoritative per-repo workspace store (ADR 0006).
//!
//! jjfx owns this model; `.jj/ws-cache` is a lossy mirror of it. The skeleton
//! tracks only a workspace's name and path, so the model is held in memory and
//! its sole persistent projection is the ws-cache. A separate on-disk store file
//! is deferred until there is state the cache cannot hold (labels, pin order,
//! forge history) - persisting only what cannot be derived, per ADR 0006.
//!
//! Existence of a workspace is the union of three sources: the always-derivable
//! `default` (its path is the repo root), the names jj reports, and the
//! `name\tpath` entries in the ws-cache. Paths come only from the ws-cache
//! (and the derived default) because jj does not record them (spike 02).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{cache, jj};

/// A single workspace. `path` is `None` when jj knows the workspace but the
/// ws-cache has no path for it (e.g. created with bare `jj workspace add`, whose
/// path the bash tools have not yet mirrored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub name: String,
    pub path: Option<PathBuf>,
}

/// The reconciled, authoritative workspace list for one repo.
#[derive(Debug, Clone)]
pub struct Store {
    pub repo_root: PathBuf,
    pub workspaces: Vec<Workspace>,
}

pub const DEFAULT_WORKSPACE: &str = "default";

/// Derive the on-disk path for a new named workspace: a sibling of the repo root
/// named `<repo>-<name>`. jj does not record workspace paths (spike 02), so jjfx
/// chooses this and persists it in the ws-cache (ADR 0006).
pub fn new_workspace_path(repo_root: &Path, name: &str) -> PathBuf {
    let base = repo_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("repo");
    let parent = repo_root.parent().unwrap_or(repo_root);
    parent.join(format!("{base}-{name}"))
}

impl Store {
    /// Load and reconcile from all live sources (jj names + ws-cache + derived
    /// default), then write the result through to the ws-cache so the bash tools
    /// stay consistent.
    pub fn load(repo_root: &Path) -> Self {
        let jj_names = jj::workspace_names(repo_root);
        let cache_entries = cache::read(&cache::path(repo_root)).unwrap_or_default();
        let workspaces = reconcile(repo_root, &cache_entries, &jj_names);
        let store = Store {
            repo_root: repo_root.to_path_buf(),
            workspaces,
        };
        // Best-effort write-through; a read-only repo must not crash the TUI.
        let _ = store.write_through_cache();
        store
    }

    /// Mirror the path-bearing workspaces to `.jj/ws-cache` atomically. Workspaces
    /// with no known path cannot be mirrored (the cache is `name\tpath`), so they
    /// are dropped from the projection - the lossiness ADR 0006 describes.
    pub fn write_through_cache(&self) -> std::io::Result<bool> {
        let entries: Vec<(String, PathBuf)> = self
            .workspaces
            .iter()
            .filter_map(|w| w.path.clone().map(|p| (w.name.clone(), p)))
            .collect();
        cache::write_through(&cache::path(&self.repo_root), &entries)
    }
}

/// Pure reconciliation - no I/O, so it is unit-testable. `default` is always
/// present with its path pinned to the repo root (authoritative, overriding any
/// stale cache entry); ws-cache paths win over jj-only names; the result is
/// ordered `default` first, then the rest alphabetically.
pub fn reconcile(
    repo_root: &Path,
    cache_entries: &[(String, PathBuf)],
    jj_names: &[String],
) -> Vec<Workspace> {
    let mut paths: BTreeMap<String, Option<PathBuf>> = BTreeMap::new();

    for name in jj_names {
        paths.entry(name.clone()).or_insert(None);
    }
    for (name, path) in cache_entries {
        paths.insert(name.clone(), Some(path.clone()));
    }
    // The default workspace's path is the repo root, by definition.
    paths.insert(DEFAULT_WORKSPACE.to_string(), Some(repo_root.to_path_buf()));

    let mut out = Vec::with_capacity(paths.len());
    if let Some(path) = paths.remove(DEFAULT_WORKSPACE) {
        out.push(Workspace {
            name: DEFAULT_WORKSPACE.to_string(),
            path,
        });
    }
    for (name, path) in paths {
        out.push(Workspace { name, path });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_always_present_first_with_repo_root_path() {
        let out = reconcile(Path::new("/repo"), &[], &[]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "default");
        assert_eq!(out[0].path, Some(PathBuf::from("/repo")));
    }

    #[test]
    fn folds_in_cache_workspaces_after_default_alphabetically() {
        let cache = vec![
            ("zeta".to_string(), PathBuf::from("/wt/zeta")),
            ("alpha".to_string(), PathBuf::from("/wt/alpha")),
        ];
        let out = reconcile(Path::new("/repo"), &cache, &[]);
        let names: Vec<_> = out.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["default", "alpha", "zeta"]);
        assert_eq!(out[1].path, Some(PathBuf::from("/wt/alpha")));
    }

    #[test]
    fn jj_only_name_appears_with_no_path() {
        // A workspace jj knows but the cache has no path for (bare `jj ws add`).
        let out = reconcile(
            Path::new("/repo"),
            &[],
            &["default".into(), "orphan".into()],
        );
        let orphan = out.iter().find(|w| w.name == "orphan").unwrap();
        assert_eq!(orphan.path, None);
    }

    #[test]
    fn cache_path_wins_over_jj_only_name() {
        let cache = vec![("feat".to_string(), PathBuf::from("/wt/feat"))];
        let out = reconcile(Path::new("/repo"), &cache, &["feat".into()]);
        let feat = out.iter().find(|w| w.name == "feat").unwrap();
        assert_eq!(feat.path, Some(PathBuf::from("/wt/feat")));
    }

    #[test]
    fn repo_root_overrides_a_stale_default_cache_entry() {
        let cache = vec![("default".to_string(), PathBuf::from("/stale/path"))];
        let out = reconcile(Path::new("/repo"), &cache, &[]);
        assert_eq!(out[0].path, Some(PathBuf::from("/repo")));
    }
}
