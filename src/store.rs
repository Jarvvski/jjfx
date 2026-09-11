//! The per-repo Workspace projection for the TUI (ADR 0006).
//!
//! The projection is read through [`wsg_core::Workspaces`], so the TUI sees the
//! same cache-backed Workspace model the CLI and Worker Pool operate on; the
//! ws-cache is a lossy mirror and never the source of truth. This module keeps
//! only the presentation facts: the default Workspace is always visible (ADR
//! 0008), a Workspace whose directory is absent has no usable path, and the
//! pending-deletion tombstone stays in memory until the runtime reports back.
//!
//! Existence of a Workspace is the union of the always-derivable `default`, the
//! names jj reports, and the `name\tpath` entries in the ws-cache - a union
//! [`wsg_core::Workspaces`] now owns.

use std::path::{Path, PathBuf};

use anyhow::Context;

use wsg_core::{Repository, WorkspaceHookStream};

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
    repo.canonicalize().unwrap()
}

/// A single workspace. `path` is `None` when no usable path is known: jj knows
/// the workspace but the ws-cache has no path for it, or the projected path's
/// directory is absent.
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
    #[cfg(test)]
    /// The normalized name accepted by jj.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    #[cfg(test)]
    /// The sibling path chosen for the new Workspace.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// The TUI's Workspace projection for one repo.
#[derive(Debug, Clone)]
pub(crate) struct Store {
    repo_root: PathBuf,
    workspaces: Vec<Workspace>,
}

pub(crate) const DEFAULT_WORKSPACE: &str = "default";

/// Derive the on-disk path for a new ad hoc workspace. The rule lives in the
/// shared library, next to the creation that uses it, so the pending tombstone
/// and the real creation can never disagree.
fn new_workspace_path(repo_root: &Path, name: &str) -> PathBuf {
    wsg_core::ad_hoc_workspace_path(repo_root, name)
}

impl Store {
    /// Load the Workspace projection through the shared cache-backed model.
    ///
    /// The read never writes the mirror; `default` is guaranteed present with its
    /// path pinned to the repository root (ADR 0008), every other entry keeps the
    /// path the mirror holds, and a Workspace whose directory is absent projects
    /// as pathless so the TUI will not target a working copy that is not there. A
    /// broken repository degrades to just the default rather than crashing the TUI.
    pub(crate) fn load(repo_root: &Path) -> Self {
        let entries = Repository::open(repo_root)
            .ok()
            .and_then(|repository| repository.workspaces().projection().ok())
            .map(|snapshot| snapshot.entries().to_vec())
            .unwrap_or_default();
        let mut workspaces: Vec<Workspace> = entries
            .into_iter()
            .map(|entry| Workspace {
                name: entry.name().to_owned(),
                path: (!entry.is_missing()).then(|| entry.path().to_path_buf()),
            })
            .collect();
        // The default Workspace is always visible, its path authoritative at the
        // repository root (ADR 0008).
        let root = repo_root.to_path_buf();
        match workspaces
            .iter_mut()
            .find(|workspace| workspace.name == DEFAULT_WORKSPACE)
        {
            Some(default) => default.path = Some(root),
            None => workspaces.insert(
                0,
                Workspace {
                    name: DEFAULT_WORKSPACE.to_owned(),
                    path: Some(root),
                },
            ),
        }
        workspaces.sort_by(|left, right| {
            (left.name != DEFAULT_WORKSPACE, &left.name)
                .cmp(&(right.name != DEFAULT_WORKSPACE, &right.name))
        });
        Store {
            repo_root: repo_root.to_path_buf(),
            workspaces,
        }
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

    #[cfg(test)]
    /// Create a workspace at the derived sibling path, project it to ws-cache,
    /// then reload the authoritative state from its live sources.
    pub(crate) fn create(&mut self, requested_name: &str) -> anyhow::Result<CreatedWorkspace> {
        let created =
            Self::create_persisted_with_progress(&self.repo_root, requested_name, |_, _| {})?;
        self.reload();
        Ok(created)
    }

    /// Create a workspace and run its repository lifecycle hook without
    /// mutating an in-memory Store. This is used by the TUI's blocking worker so
    /// setup does not pause input handling.
    pub(crate) fn create_persisted_with_progress<F>(
        repo_root: &Path,
        requested_name: &str,
        on_output: F,
    ) -> anyhow::Result<CreatedWorkspace>
    where
        F: FnMut(WorkspaceHookStream, &str),
    {
        let name = requested_name.trim();
        if name.is_empty() {
            anyhow::bail!("workspace name required");
        }

        let repository = Repository::open(repo_root).context("create failed")?;
        let trunk = crate::trunk::as_revset();
        let workspace = repository
            .create_ad_hoc_workspace_with_revision_and_progress(name, Some(&trunk), on_output)
            .context("create failed")?;
        Ok(CreatedWorkspace {
            name: workspace.name().to_owned(),
            path: workspace.path().to_owned(),
        })
    }

    /// Return the sibling path used for a new named workspace.
    pub(crate) fn new_workspace_path(&self, name: &str) -> PathBuf {
        new_workspace_path(&self.repo_root, name)
    }

    /// Reconcile the Store from jj and the ws-cache mirror.
    pub(crate) fn reload(&mut self) {
        let repo_root = self.repo_root.clone();
        *self = Self::load(&repo_root);
    }

    /// Forget a workspace in jj, clean up its guarded directory and cache entry,
    /// then reload the authoritative state from its live sources.
    #[cfg(test)]
    pub(crate) fn delete(&mut self, name: &str) -> anyhow::Result<()> {
        let path = self
            .workspace(name)
            .and_then(|workspace| workspace.path.clone());
        Self::delete_persisted(&self.repo_root, name, path.as_deref())?;
        self.reload();
        Ok(())
    }

    /// Delete one workspace's persistent state without replacing this Store.
    /// Callers running outside the App task can then load a fresh projection,
    /// including any partial changes if a later cleanup phase fails.
    pub(crate) fn delete_persisted(
        repo_root: &Path,
        name: &str,
        known_path: Option<&Path>,
    ) -> anyhow::Result<()> {
        if name == DEFAULT_WORKSPACE {
            anyhow::bail!("the default workspace cannot be deleted");
        }
        let repository = Repository::open(repo_root).context("delete failed")?;
        repository
            .remove_ad_hoc_workspace(name, known_path)
            .context("delete failed")?;
        Ok(())
    }

    /// Keep a pending deletion visible in the App's in-memory projection after
    /// a watcher reload. The tombstone is never written back to the cache.
    pub(crate) fn restore_workspace(&mut self, workspace: Workspace) {
        if self.workspace(&workspace.name).is_some() {
            return;
        }
        self.workspaces.push(workspace);
        self.workspaces.sort_by(|left, right| {
            (left.name != DEFAULT_WORKSPACE, &left.name)
                .cmp(&(right.name != DEFAULT_WORKSPACE, &right.name))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jj;

    /// Write the compatible ws-cache byte format directly, as the bash tools do.
    fn write_cache(repo: &Path, entries: &[(&str, &Path)]) {
        let body: String = entries
            .iter()
            .map(|(name, path)| format!("{name}\t{}\n", path.display()))
            .collect();
        std::fs::write(repo.join(".jj").join("ws-cache"), body).unwrap();
    }

    #[test]
    fn load_orders_cache_workspaces_and_pins_default_to_repo_root() {
        let repo = test_local_repo("load-cache");
        let alpha = repo.with_file_name("alpha-workspace");
        let zeta = repo.with_file_name("zeta-workspace");
        std::fs::create_dir_all(&alpha).unwrap();
        std::fs::create_dir_all(&zeta).unwrap();
        write_cache(
            &repo,
            &[
                ("zeta", zeta.as_path()),
                (DEFAULT_WORKSPACE, Path::new("/stale/path")),
                ("alpha", alpha.as_path()),
            ],
        );

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

        std::fs::remove_dir_all(&alpha).unwrap();
        std::fs::remove_dir_all(&zeta).unwrap();
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
    fn store_projection_agrees_with_the_shared_workspace_projection() {
        use std::collections::BTreeSet;
        use wsg_core::WorkspaceAddOutcome;

        let repo = test_local_repo("store-agrees");
        let repository = Repository::open(&repo).unwrap();
        let WorkspaceAddOutcome::Created(created) = repository
            .workspaces()
            .add("feature", None)
            .expect("workspace should be added")
        else {
            panic!("workspace should be created");
        };

        let store = Store::load(&repo);
        let shared = repository.workspaces().snapshot().unwrap();

        let tui: BTreeSet<(String, Option<PathBuf>)> = store
            .workspaces()
            .iter()
            .map(|workspace| (workspace.name.clone(), workspace.path.clone()))
            .collect();
        let cli: BTreeSet<(String, Option<PathBuf>)> = shared
            .entries()
            .iter()
            .map(|entry| {
                (
                    entry.name().to_owned(),
                    (!entry.is_missing()).then(|| entry.path().to_path_buf()),
                )
            })
            .collect();
        assert_eq!(tui, cli);
        assert_eq!(
            store.workspace("feature").unwrap().path.as_deref(),
            Some(created.path())
        );

        std::fs::remove_dir_all(created.path()).unwrap();
        std::fs::remove_dir_all(&repo).unwrap();
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
    fn create_uses_canonical_trunk_when_default_is_elsewhere() {
        use std::process::Command;

        let repo = test_local_repo("create-canonical-trunk");
        let run_jj = |args: &[&str]| {
            let output = Command::new("jj")
                .args(["--config", "signing.behavior=drop"])
                .args(args)
                .current_dir(&repo)
                .output()
                .expect("jj should run");
            assert!(
                output.status.success(),
                "jj command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run_jj(&["new", "-m", "mainline"]);
        run_jj(&["bookmark", "set", "main"]);
        run_jj(&["new", "root()"]);

        let mut store = Store::load(&repo);
        let created = store.create("feat").expect("workspace should be created");
        let parent = Command::new("jj")
            .args([
                "--config",
                "signing.behavior=drop",
                "log",
                "-r",
                "feat@-",
                "--no-graph",
                "-T",
                "bookmarks ++ \"\\n\"",
            ])
            .current_dir(&repo)
            .output()
            .expect("jj should inspect the workspace parent");
        assert!(
            parent.status.success(),
            "jj parent inspection failed: {}",
            String::from_utf8_lossy(&parent.stderr)
        );
        assert_eq!(
            String::from_utf8(parent.stdout).expect("jj output should be UTF-8"),
            "main\n"
        );

        std::fs::remove_dir_all(created.path()).unwrap();
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
        let cache_path = repo.join(".jj").join("ws-cache");
        if cache_path.exists() {
            std::fs::remove_file(&cache_path).unwrap();
        }
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
    fn delete_jj_failure_continues_filesystem_cleanup() {
        let repo = test_local_repo("delete-jj-failure");
        let path = new_workspace_path(&repo, "ghost");
        std::fs::create_dir(&path).unwrap();
        write_cache(
            &repo,
            &[
                (DEFAULT_WORKSPACE, repo.as_path()),
                ("ghost", path.as_path()),
            ],
        );
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
        assert!(!path.exists());
        std::fs::rename(&disabled_repo_state, &repo_state).unwrap();
        let fresh = Store::load(&repo);
        assert!(fresh.workspace("ghost").is_none());

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
