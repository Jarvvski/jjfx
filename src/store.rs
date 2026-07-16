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

use anyhow::Context;

use crate::{cache, jj};

#[cfg(test)]
/// Create an isolated local jj repository with signing disabled for tests.
pub(crate) fn test_local_repo(tag: &str) -> PathBuf {
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let repo =
        std::env::temp_dir().join(format!("jjfx-store-{tag}-{}-{nonce}", std::process::id()));
    let output = Command::new("jj")
        .args(["--config", "signing.behavior=drop", "git", "init"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "jj git init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    repo
}

/// A single workspace. `path` is `None` when jj knows the workspace but the
/// ws-cache has no path for it (e.g. created with bare `jj workspace add`, whose
/// path the bash tools have not yet mirrored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Workspace {
    pub(crate) name: String,
    pub(crate) path: Option<PathBuf>,
}

/// A newly-created workspace, whose path is always known because jjfx chose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CreatedWorkspace {
    name: String,
    path: PathBuf,
}

impl CreatedWorkspace {
    /// The normalized name accepted by jj.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// The sibling path chosen for the new Workspace.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// The reconciled, authoritative workspace list for one repo.
#[derive(Debug, Clone)]
pub(crate) struct Store {
    repo_root: PathBuf,
    workspaces: Vec<Workspace>,
}

pub(crate) const DEFAULT_WORKSPACE: &str = "default";

/// Derive the on-disk path for a new named workspace: a sibling of the repo root
/// named `<repo>-<name>`. jj does not record workspace paths (spike 02), so jjfx
/// chooses this and persists it in the ws-cache (ADR 0006).
fn new_workspace_path(repo_root: &Path, name: &str) -> PathBuf {
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
    pub(crate) fn load(repo_root: &Path) -> Self {
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

    /// The repository whose Workspace state this Store owns.
    pub(crate) fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    /// The reconciled Workspaces, ordered with default first then by name.
    pub(crate) fn workspaces(&self) -> &[Workspace] {
        &self.workspaces
    }

    /// Find one Workspace by its stable jj name.
    pub(crate) fn workspace(&self, name: &str) -> Option<&Workspace> {
        self.workspaces
            .iter()
            .find(|workspace| workspace.name == name)
    }

    #[cfg(test)]
    /// Construct an in-memory snapshot for App tests unrelated to persistence.
    pub(crate) fn from_workspaces_for_test(repo_root: PathBuf, workspaces: Vec<Workspace>) -> Self {
        Self {
            repo_root,
            workspaces,
        }
    }

    /// Create a workspace at the derived sibling path, project it to ws-cache,
    /// then reload the authoritative state from its live sources.
    pub(crate) fn create(&mut self, requested_name: &str) -> anyhow::Result<CreatedWorkspace> {
        let name = requested_name.trim();
        if name.is_empty() {
            anyhow::bail!("workspace name required");
        }
        if self.workspace(name).is_some() {
            anyhow::bail!("workspace '{name}' already exists");
        }

        let path = new_workspace_path(&self.repo_root, name);
        jj::add_workspace(&self.repo_root, name, &path).context("create failed")?;
        let cache_path = cache::path(&self.repo_root);
        let mut entries = cache::read(&cache_path).unwrap_or_default();
        if !entries.iter().any(|(entry_name, _)| entry_name == name) {
            entries.push((name.to_string(), path.clone()));
        }
        // The mirror is deliberately lossy and best-effort (ADR 0006). jj is
        // already authoritative for existence, so projection failure must not
        // turn a successfully-created workspace into a reported failure.
        let _ = cache::write_through(&cache_path, &entries);
        self.reload();
        Ok(CreatedWorkspace {
            name: name.to_string(),
            path,
        })
    }

    /// Reconcile the Store from jj and the ws-cache mirror.
    pub(crate) fn reload(&mut self) {
        let repo_root = self.repo_root.clone();
        *self = Self::load(&repo_root);
    }

    /// Forget a workspace in jj, clean up its guarded directory and cache entry,
    /// then reload the authoritative state from its live sources.
    pub(crate) fn delete(&mut self, name: &str) -> anyhow::Result<()> {
        if name == DEFAULT_WORKSPACE {
            anyhow::bail!("the default workspace cannot be deleted");
        }
        let path = self
            .workspace(name)
            .and_then(|workspace| workspace.path.clone());
        jj::forget_workspace(&self.repo_root, name).context("delete failed")?;
        if let Some(path) = path
            && path != self.repo_root
            && path.is_dir()
        {
            let _ = std::fs::remove_dir_all(path);
        }
        let cache_path = cache::path(&self.repo_root);
        let entries: Vec<_> = cache::read(&cache_path)
            .unwrap_or_default()
            .into_iter()
            .filter(|(entry_name, _)| entry_name != name)
            .collect();
        let _ = cache::write_through(&cache_path, &entries);
        self.reload();
        Ok(())
    }

    /// Mirror the path-bearing workspaces to `.jj/ws-cache` atomically. Workspaces
    /// with no known path cannot be mirrored (the cache is `name\tpath`), so they
    /// are dropped from the projection - the lossiness ADR 0006 describes.
    fn write_through_cache(&self) -> std::io::Result<bool> {
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
fn reconcile(
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
    fn load_orders_cache_workspaces_and_pins_default_to_repo_root() {
        let repo = test_local_repo("load-cache");
        let alpha = repo.with_file_name("alpha-workspace");
        let zeta = repo.with_file_name("zeta-workspace");
        cache::write_through(
            &cache::path(&repo),
            &[
                ("zeta".to_string(), zeta),
                (DEFAULT_WORKSPACE.to_string(), PathBuf::from("/stale/path")),
                ("alpha".to_string(), alpha.clone()),
            ],
        )
        .unwrap();

        let store = Store::load(&repo);
        let names: Vec<_> = store
            .workspaces()
            .iter()
            .map(|workspace| workspace.name.as_str())
            .collect();

        assert_eq!(names, ["default", "alpha", "zeta"]);
        assert_eq!(
            store.workspace(DEFAULT_WORKSPACE).unwrap().path.as_deref(),
            Some(repo.as_path())
        );
        assert_eq!(
            store.workspace("alpha").unwrap().path.as_deref(),
            Some(alpha.as_path())
        );

        std::fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn load_represents_jj_only_workspace_without_path() {
        let repo = test_local_repo("load-jj-only");
        let path = repo.with_file_name("orphan-workspace");
        jj::add_workspace(&repo, "orphan", &path).unwrap();

        let store = Store::load(&repo);

        assert_eq!(store.workspace("orphan").unwrap().path, None);

        std::fs::remove_dir_all(path).unwrap();
        std::fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn create_jj_failure_preserves_loaded_store() {
        let repo = test_local_repo("create-jj-failure");
        let path = new_workspace_path(&repo, "feat");
        let mut store = Store::load(&repo);
        let before = store.workspaces().to_vec();

        // Make Store's snapshot stale: jj knows `feat`, but Store does not. The
        // attempted duplicate therefore reaches jj and fails at the critical step.
        jj::add_workspace(&repo, "feat", &path).unwrap();

        let error = store.create("feat").unwrap_err();

        assert!(format!("{error:#}").starts_with("create failed:"));
        assert_eq!(store.workspaces(), before);
        let fresh = Store::load(&repo);
        assert_eq!(fresh.workspace("feat").unwrap().path, None);

        std::fs::remove_dir_all(&path).unwrap();
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn create_reconciles_jj_and_cache_state() {
        let repo = test_local_repo("create-success");
        let mut store = Store::load(&repo);

        let created = store.create(" feat ").unwrap();
        let expected_path = new_workspace_path(&repo, "feat");

        assert_eq!(created.name(), "feat");
        assert_eq!(created.path(), expected_path);
        assert_eq!(
            store.workspace("feat").unwrap().path.as_deref(),
            Some(expected_path.as_path())
        );
        let fresh = Store::load(&repo);
        assert_eq!(
            fresh.workspace("feat").unwrap().path.as_deref(),
            Some(expected_path.as_path())
        );

        std::fs::remove_dir_all(&expected_path).unwrap();
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn create_survives_cache_projection_failure() {
        let repo = test_local_repo("create-cache-failure");
        let mut store = Store::load(&repo);
        let cache_path = cache::path(&repo);
        std::fs::remove_file(&cache_path).unwrap();
        std::fs::create_dir(&cache_path).unwrap();

        let created = store.create("feat").unwrap();
        let expected_path = new_workspace_path(&repo, "feat");

        assert_eq!(created.name(), "feat");
        assert_eq!(created.path(), expected_path);
        assert_eq!(store.workspace("feat").unwrap().path, None);

        std::fs::remove_dir_all(&expected_path).unwrap();
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn delete_jj_failure_preserves_store_cache_and_directory() {
        let repo = test_local_repo("delete-jj-failure");
        let path = new_workspace_path(&repo, "ghost");
        std::fs::create_dir(&path).unwrap();
        cache::write_through(
            &cache::path(&repo),
            &[
                (DEFAULT_WORKSPACE.to_string(), repo.clone()),
                ("ghost".to_string(), path.clone()),
            ],
        )
        .unwrap();
        let mut store = Store::load(&repo);
        let repo_state = repo.join(".jj").join("repo");
        let disabled_repo_state = repo.join(".jj").join("repo-disabled");
        std::fs::rename(&repo_state, &disabled_repo_state).unwrap();

        let error = store.delete("ghost").unwrap_err();

        assert!(format!("{error:#}").starts_with("delete failed:"));
        assert_eq!(
            store.workspace("ghost").unwrap().path.as_deref(),
            Some(path.as_path())
        );
        assert!(path.is_dir());
        std::fs::rename(&disabled_repo_state, &repo_state).unwrap();
        let fresh = Store::load(&repo);
        assert_eq!(
            fresh.workspace("ghost").unwrap().path.as_deref(),
            Some(path.as_path())
        );

        std::fs::remove_dir_all(&path).unwrap();
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn delete_removes_directory_cache_and_store_entry() {
        let repo = test_local_repo("delete-success");
        let mut store = Store::load(&repo);
        let created = store.create("feat").unwrap();
        let path = created.path().to_path_buf();

        store.delete("feat").unwrap();

        assert!(store.workspace("feat").is_none());
        assert!(!path.exists());
        let fresh = Store::load(&repo);
        assert!(fresh.workspace("feat").is_none());

        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn delete_rejects_default_and_preserves_repo_root() {
        let repo = test_local_repo("delete-default");
        let mut store = Store::load(&repo);

        let error = store.delete(DEFAULT_WORKSPACE).unwrap_err();

        assert_eq!(
            format!("{error:#}"),
            "the default workspace cannot be deleted"
        );
        assert!(repo.is_dir());
        assert_eq!(
            store.workspace(DEFAULT_WORKSPACE).unwrap().path.as_deref(),
            Some(repo.as_path())
        );

        std::fs::remove_dir_all(&repo).unwrap();
    }
}
