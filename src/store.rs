//! The per-repo Workspace projection for the TUI (ADR 0006).
//!
//! The projection is read through [`crate::engine::Workspaces`], so the TUI sees the
//! same cache-backed Workspace model the CLI and Worker Pool operate on; the
//! ws-cache is a lossy mirror and never the source of truth. This module keeps
//! only the presentation facts: the default Workspace is always visible (ADR
//! 0008), a Workspace whose directory is absent has no usable path, and the
//! pending-deletion tombstone stays in memory until the runtime reports back.
//!
//! Existence of a Workspace is the union of the always-derivable `default`, the
//! names jj reports, and the `name\tpath` entries in the ws-cache - a union
//! [`crate::engine::Workspaces`] now owns.

use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::engine::{AdHocWorkspacePhase, Repository, WorkspaceHookStream};

#[cfg(test)]
/// Create an isolated local jj repository with signing disabled and its own jj
/// config for tests.
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
        .args([
            "--config",
            "signing.behavior=drop",
            "git",
            "init",
            "--no-colocate",
        ])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "jj git init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    write_repo_jj_config(&repo, false);
    repo.canonicalize().unwrap()
}

#[cfg(test)]
/// Write a throwaway repository's *own* jj config: a test identity and an
/// auto-tracking choice, so the repository never inherits the developer's
/// `~/.config/jj/config.toml` (see
/// [`crate::test_support::hermetic_jj_config`]).
///
/// It is written to the path jj itself reports for the repository config
/// (`jj config path --repo`), not the deprecated `.jj/repo/config.toml`, so jj
/// reads it directly and no migration warning reaches a test's captured stderr.
pub(crate) fn write_repo_jj_config(root: &Path, auto_track_origin: bool) {
    let config = repo_config_path(root);
    std::fs::create_dir_all(config.parent().expect("config has a parent"))
        .expect("repo config directory");
    std::fs::write(
        &config,
        crate::test_support::hermetic_jj_config(auto_track_origin),
    )
    .expect("repo-local jj config");
}

#[cfg(test)]
/// The path jj stores `root`'s repository config at, read from jj rather than
/// derived, because jj keys it by the repository's own config id.
fn repo_config_path(root: &Path) -> PathBuf {
    let output = crate::jj::command()
        .arg("--repository")
        .arg(root)
        .args(["config", "path", "--repo"])
        .output()
        .expect("jj should be installed");
    assert!(
        output.status.success(),
        "jj config path --repo failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
}

#[cfg(test)]
/// Make a repository's `origin` auto-track every bookmark, in the repository's
/// *own* config.
///
/// A bookmark created after this is written gets a `remote=origin present=false`
/// remote-tracking entry even though no remote ref exists, so jj lists it twice
/// under the plain `name` template. That double-listing is a *configuration*
/// (`remotes.origin.auto-track-bookmarks`), not a jj default: with a clean jj
/// config the same bookmark lists once. A test that depends on it must therefore
/// construct it here rather than inherit it from the developer's
/// `~/.config/jj/config.toml`, or it passes only on a machine that sets the
/// option and fails on a clean-config CI runner (dogfood-2 02).
pub(crate) fn track_bookmarks_from_origin(root: &Path) {
    write_repo_jj_config(root, true);
}

/// A single workspace. `path` records where the workspace's directory is, as
/// far as the projection can tell: a recorded path whose directory is gone is
/// [`WorkspacePath::Missing`] (the Workspace moved or was removed), while a
/// derived path that never resolved is [`WorkspacePath::Unknown`] (jjfx never
/// knew where it lived).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Workspace {
    pub(crate) name: String,
    pub(crate) path: WorkspacePath,
}

/// Where a Workspace's working copy is, and whether jjfx can use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkspacePath {
    /// The path is known and its directory exists.
    Present(PathBuf),
    /// A recorded path whose directory is absent.
    Missing(PathBuf),
    /// No path is recorded and the naming convention did not resolve to a
    /// directory, so jjfx does not know where this Workspace lives.
    Unknown,
}

impl WorkspacePath {
    /// The path when its directory exists. `None` for both a missing directory
    /// and an unknown path, so callers that act on a working copy keep treating
    /// every unusable path the same way.
    pub(crate) fn present(&self) -> Option<&Path> {
        match self {
            WorkspacePath::Present(path) => Some(path),
            WorkspacePath::Missing(_) | WorkspacePath::Unknown => None,
        }
    }
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

/// A failed Workspace creation, tagged with the lifecycle phase so the TUI can
/// say whether anything was created: `Create` means nothing was; `Hook` means the
/// Workspace exists but its setup did not finish.
#[derive(Debug)]
pub(crate) struct WorkspaceCreateError {
    pub(crate) phase: AdHocWorkspacePhase,
    pub(crate) message: String,
}

impl std::fmt::Display for WorkspaceCreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WorkspaceCreateError {}

impl WorkspaceCreateError {
    /// A creation-step failure carrying `message` (nothing was created).
    pub(crate) fn create(message: impl Into<String>) -> Self {
        Self {
            phase: AdHocWorkspacePhase::Create,
            message: message.into(),
        }
    }
}

/// The TUI's Workspace projection for one repo.
#[derive(Debug, Clone)]
pub(crate) struct Store {
    repo_root: PathBuf,
    workspaces: Vec<Workspace>,
    /// Whether new workspaces are colocated with Git (jjfx's `colocate`).
    colocate: bool,
}

pub(crate) const DEFAULT_WORKSPACE: &str = "default";

/// Derive the on-disk path for a new ad hoc workspace. The rule lives in the
/// shared library, next to the creation that uses it, so the pending tombstone
/// and the real creation can never disagree.
fn new_workspace_path(repo_root: &Path, name: &str) -> PathBuf {
    crate::engine::ad_hoc_workspace_path(repo_root, name)
}

impl Store {
    /// Load the Workspace projection through the shared cache-backed model.
    ///
    /// The read never writes the mirror; `default` is guaranteed present with its
    /// path pinned to the repository root (ADR 0008), every other entry keeps the
    /// path the mirror holds, and a Workspace whose directory is absent projects
    /// as unusable so the TUI will not target a working copy that is not there. A
    /// broken repository degrades to just the default rather than crashing the TUI.
    pub(crate) fn load(repo_root: &Path) -> Self {
        let repository = Repository::open(repo_root).ok();
        // Normalize to the repository's default workspace root: a secondary
        // workspace passed in must not shadow `default` (ADR 0008).
        let root = repository.as_ref().map_or_else(
            || repo_root.to_path_buf(),
            |repository| repository.root().to_path_buf(),
        );
        let entries = repository
            .and_then(|repository| repository.workspaces().projection().ok())
            .map(|snapshot| snapshot.entries().to_vec())
            .unwrap_or_default();
        let mut workspaces: Vec<Workspace> = entries
            .into_iter()
            .map(|entry| Workspace {
                name: entry.name().to_owned(),
                path: match entry.path() {
                    Some(path) if path.is_dir() => WorkspacePath::Present(path.to_path_buf()),
                    Some(path) => WorkspacePath::Missing(path.to_path_buf()),
                    None => WorkspacePath::Unknown,
                },
            })
            .collect();
        // The default Workspace is always visible, its path authoritative at the
        // repository root (ADR 0008).
        match workspaces
            .iter_mut()
            .find(|workspace| workspace.name == DEFAULT_WORKSPACE)
        {
            Some(default) => default.path = WorkspacePath::Present(root.clone()),
            None => workspaces.insert(
                0,
                Workspace {
                    name: DEFAULT_WORKSPACE.to_owned(),
                    path: WorkspacePath::Present(root.clone()),
                },
            ),
        }
        workspaces.sort_by(|left, right| {
            (left.name != DEFAULT_WORKSPACE, &left.name)
                .cmp(&(right.name != DEFAULT_WORKSPACE, &right.name))
        });
        Store {
            repo_root: root,
            workspaces,
            colocate: true,
        }
    }

    /// The repository whose Workspace state this Store owns.
    pub(crate) fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    /// Whether new workspaces are colocated with Git.
    pub(crate) fn colocate(&self) -> bool {
        self.colocate
    }

    /// Sets the Workspace colocation preference (builder form).
    pub(crate) fn with_colocate(mut self, colocate: bool) -> Self {
        self.colocate = colocate;
        self
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
            colocate: true,
        }
    }

    #[cfg(test)]
    /// Create a workspace at the derived sibling path, project it to ws-cache,
    /// then reload the authoritative state from its live sources.
    pub(crate) fn create(&mut self, requested_name: &str) -> anyhow::Result<CreatedWorkspace> {
        let created = Self::create_persisted_with_progress(
            &self.repo_root,
            requested_name,
            self.colocate,
            |_, _| {},
        )?;
        let repo_root = self.repo_root.clone();
        *self = Self::load(&repo_root);
        Ok(created)
    }

    /// Create a workspace and run its repository lifecycle hook without
    /// mutating an in-memory Store. This is used by the TUI's blocking worker so
    /// setup does not pause input handling.
    pub(crate) fn create_persisted_with_progress<F>(
        repo_root: &Path,
        requested_name: &str,
        colocate: bool,
        on_output: F,
    ) -> Result<CreatedWorkspace, WorkspaceCreateError>
    where
        F: FnMut(WorkspaceHookStream, &str),
    {
        let name = requested_name.trim();
        if name.is_empty() {
            return Err(WorkspaceCreateError {
                phase: AdHocWorkspacePhase::Create,
                message: "workspace name required".to_string(),
            });
        }

        let repository = Repository::open(repo_root)
            .map_err(|error| WorkspaceCreateError {
                phase: AdHocWorkspacePhase::Create,
                message: format!("{error:#}"),
            })?
            .with_colocate(colocate);
        let trunk = crate::trunk::as_revset();
        let workspace = repository
            .create_ad_hoc_workspace_with_revision_and_progress(name, Some(&trunk), on_output)
            .map_err(|error| WorkspaceCreateError {
                phase: error.phase(),
                message: error.to_string(),
            })?;
        Ok(CreatedWorkspace {
            name: workspace.name().to_owned(),
            path: workspace.path().to_owned(),
        })
    }

    /// Return the sibling path used for a new named workspace.
    pub(crate) fn new_workspace_path(&self, name: &str) -> PathBuf {
        new_workspace_path(&self.repo_root, name)
    }

    /// Forget a workspace in jj, clean up its guarded directory and cache entry,
    /// then reconcile the authoritative state from its live sources.
    #[cfg(test)]
    pub(crate) fn delete(&mut self, name: &str) -> anyhow::Result<()> {
        let path = self
            .workspace(name)
            .and_then(|workspace| workspace.path.present().map(Path::to_path_buf));
        Self::delete_persisted(&self.repo_root, name, path.as_deref())?;
        let repo_root = self.repo_root.clone();
        *self = Self::load(&repo_root);
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

    /// A sibling of the unique `test_local_repo` directory, named from that
    /// directory so concurrent test runs never share a path.
    fn sibling(repo: &Path, tag: &str) -> PathBuf {
        repo.with_file_name(format!(
            "{}-{tag}",
            repo.file_name().unwrap().to_string_lossy()
        ))
    }

    /// Runs a jj command in `repo`, asserting it succeeds.
    fn jj_in(repo: &Path, args: &[&str]) {
        let output = jj::command()
            .current_dir(repo)
            .args(args)
            .output()
            .expect("jj should be installed");
        assert!(
            output.status.success(),
            "jj {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A throwaway repository owns its jj config, so a fixture never inherits
    /// the developer's `[user]` identity or auto-tracking (bug-hunt 45).
    ///
    /// A unit test cannot set `JJ_CONFIG` on the `jj` processes jjfx spawns, so
    /// the repository carries its own config instead. This asserts that config
    /// wins over the developer's, whatever it says - without it a commit or a
    /// delivery-identity read would pass locally and fail on a clean-config
    /// runner.
    #[test]
    fn a_throwaway_repo_owns_its_jj_identity_and_auto_tracking() {
        let repo = test_local_repo("hermetic-config");
        let email = jj::read_at_repo(&repo, &["config", "get", "user.email"])
            .expect("jj reports the repository's identity");
        assert_eq!(
            email.trim(),
            "jjfx@example.com",
            "the repository's own identity must win over the developer's"
        );

        // Auto-tracking is off by default, so a fresh bookmark lists once rather
        // than twice (the tracked-but-absent shape dogfood-2 02 fixed). A remote
        // has to exist for the setting to have anything to track.
        jj_in(
            &repo,
            &[
                "git",
                "remote",
                "add",
                "origin",
                "git@github.com:owner/name.git",
            ],
        );
        jj_in(&repo, &["bookmark", "create", "fresh", "-r", "@"]);
        let listed = jj::read_at_repo(&repo, &["bookmark", "list", "-T", "name ++ \"\\n\""])
            .expect("bookmark list");
        assert_eq!(
            listed.lines().filter(|line| *line == "fresh").count(),
            1,
            "a fresh bookmark must list once, not inherit ambient auto-tracking: {listed}"
        );
    }

    /// [`track_bookmarks_from_origin`] is the opt-in that flips the default:
    /// a bookmark created afterwards lists twice, once per remote-tracking
    /// entry. This is the state the dogfood-2 02 tests assert against.
    #[test]
    fn track_bookmarks_from_origin_turns_the_repo_own_auto_tracking_on() {
        let repo = test_local_repo("hermetic-track");
        track_bookmarks_from_origin(&repo);
        jj_in(
            &repo,
            &[
                "git",
                "remote",
                "add",
                "origin",
                "git@github.com:owner/name.git",
            ],
        );
        jj_in(&repo, &["bookmark", "create", "fresh", "-r", "@"]);
        let listed = jj::read_at_repo(&repo, &["bookmark", "list", "-T", "name ++ \"\\n\""])
            .expect("bookmark list");
        assert_eq!(
            listed.lines().filter(|line| *line == "fresh").count(),
            2,
            "an opted-in repository must list the fresh bookmark twice: {listed}"
        );
    }

    #[test]
    fn load_orders_jj_workspaces_and_pins_default_to_repo_root() {
        let repo = test_local_repo("load-order");
        let alpha = sibling(&repo, "alpha-workspace");
        let zeta = sibling(&repo, "zeta-workspace");
        // Created out of alphabetical order, at off-convention paths: the store
        // takes the set and the paths from jj, and orders by name itself.
        jj::add_workspace(&repo, "zeta", &zeta).unwrap();
        jj::add_workspace(&repo, "alpha", &alpha).unwrap();

        let store = Store::load(&repo);
        let names: Vec<_> = store
            .workspaces()
            .iter()
            .map(|workspace| workspace.name.as_str())
            .collect();

        assert_eq!(names, ["default", "alpha", "zeta"]);
        assert_eq!(
            store.workspace(DEFAULT_WORKSPACE).unwrap().path.present(),
            Some(repo.as_path())
        );
        assert_eq!(
            store.workspace("alpha").unwrap().path.present(),
            Some(alpha.as_path())
        );

        std::fs::remove_dir_all(&alpha).unwrap();
        std::fs::remove_dir_all(&zeta).unwrap();
        std::fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn load_from_a_secondary_workspace_pins_default_to_the_repository_root() {
        let tmp = std::env::temp_dir().join(format!("jjfx-store-secondary-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let default = tmp.join("repo");
        let secondary = tmp.join("ws-secondary");
        // The default workspace owns the store; a secondary workspace points its
        // `.jj/repo` file at it (exactly what `jj workspace add` writes).
        std::fs::create_dir_all(default.join(".jj/repo")).unwrap();
        std::fs::create_dir_all(secondary.join(".jj")).unwrap();
        std::fs::write(secondary.join(".jj/repo"), "../../repo/.jj/repo").unwrap();

        let store = Store::load(&secondary);

        let expected = default.canonicalize().unwrap();
        assert_eq!(store.repo_root(), expected.as_path());
        assert_eq!(
            store.workspace(DEFAULT_WORKSPACE).unwrap().path.present(),
            Some(expected.as_path())
        );

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn load_reads_a_jj_workspace_path_from_jj() {
        let repo = test_local_repo("load-jj-path");
        let path = sibling(&repo, "off-convention-workspace");
        jj::add_workspace(&repo, "elsewhere", &path).unwrap();

        let store = Store::load(&repo);

        // jj records where the Workspace is, so jjfx reads it instead of guessing
        // from the naming convention - which points somewhere that never existed.
        assert_eq!(
            store.workspace("elsewhere").unwrap().path,
            WorkspacePath::Present(path.clone())
        );

        std::fs::remove_dir_all(path).unwrap();
        std::fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn load_reports_a_jj_workspace_whose_directory_is_gone_as_missing() {
        let repo = test_local_repo("load-missing-dir");
        let path = sibling(&repo, "gone-workspace");
        jj::add_workspace(&repo, "gone", &path).unwrap();
        std::fs::remove_dir_all(&path).unwrap();

        let store = Store::load(&repo);

        // jj still reports the Workspace and records where it was, so its absent
        // directory means it moved or was removed - not that the path is unknown.
        assert_eq!(
            store.workspace("gone").unwrap().path,
            WorkspacePath::Missing(path.clone())
        );
        assert_eq!(store.workspace("gone").unwrap().path.present(), None);

        std::fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn refresh_mirrors_the_paths_jj_records() {
        let repo = test_local_repo("refresh-mirror");
        let path = sibling(&repo, "elsewhere-workspace");
        jj::add_workspace(&repo, "elsewhere", &path).unwrap();

        let repository = Repository::open(&repo).unwrap();
        repository.workspaces().refresh().unwrap();

        // The mirror is written for the tools that read it, so it holds the path
        // jj records - not a naming-convention guess.
        let cache = std::fs::read_to_string(repo.join(".jj/ws-cache")).unwrap();
        assert!(
            cache.contains(&format!("elsewhere\t{}", path.display())),
            "{cache}"
        );

        std::fs::remove_dir_all(path).unwrap();
        std::fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn store_projection_agrees_with_the_shared_workspace_projection() {
        use crate::engine::WorkspaceAddOutcome;
        use std::collections::BTreeSet;

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
            .map(|workspace| {
                (
                    workspace.name.clone(),
                    workspace.path.present().map(Path::to_path_buf),
                )
            })
            .collect();
        let cli: BTreeSet<(String, Option<PathBuf>)> = shared
            .entries()
            .iter()
            .map(|entry| {
                (
                    entry.name().to_owned(),
                    entry
                        .path()
                        .filter(|path| path.is_dir())
                        .map(Path::to_path_buf),
                )
            })
            .collect();
        assert_eq!(tui, cli);
        assert_eq!(
            store.workspace("feature").unwrap().path.present(),
            Some(created.path())
        );

        std::fs::remove_dir_all(created.path()).unwrap();
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn create_jj_failure_preserves_loaded_store() {
        let repo = test_local_repo("create-jj-failure");
        let path = new_workspace_path(&repo, "feat");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut store = Store::load(&repo);
        let before = store.workspaces().to_vec();

        // Make Store's snapshot stale: jj knows `feat`, but Store does not. The
        // attempted duplicate therefore reaches jj and fails at the critical step.
        jj::add_workspace(&repo, "feat", &path).unwrap();

        let error = store.create("feat").unwrap_err();

        assert!(format!("{error:#}").contains("already exists"));
        assert_eq!(store.workspaces(), before);
        let fresh = Store::load(&repo);
        assert_eq!(
            fresh.workspace("feat").unwrap().path.present(),
            Some(path.as_path())
        );

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
            store.workspace("feat").unwrap().path.present(),
            Some(expected_path.as_path())
        );
        let fresh = Store::load(&repo);
        assert_eq!(
            fresh.workspace("feat").unwrap().path.present(),
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
        assert_eq!(
            store.workspace("feat").unwrap().path.present(),
            Some(expected_path.as_path())
        );

        std::fs::remove_dir_all(&expected_path).unwrap();
        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn delete_jj_failure_continues_filesystem_cleanup() {
        let repo = test_local_repo("delete-jj-failure");
        let mut store = Store::load(&repo);
        let created = store.create("ghost").unwrap();
        let path = created.path().to_path_buf();
        let repo_state = repo.join(".jj").join("repo");
        let disabled_repo_state = repo.join(".jj").join("repo-disabled");
        std::fs::rename(&repo_state, &disabled_repo_state).unwrap();

        let error = store.delete("ghost").unwrap_err();

        assert!(format!("{error:#}").starts_with("delete failed:"));
        assert_eq!(
            store.workspace("ghost").unwrap().path.present(),
            Some(path.as_path())
        );
        assert!(!path.exists(), "the directory is cleaned up regardless");
        std::fs::rename(&disabled_repo_state, &repo_state).unwrap();
        // jj never forgot it, so jj - the source of truth for Workspaces - still
        // reports it.
        let fresh = Store::load(&repo);
        assert!(fresh.workspace("ghost").is_some());

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
            store.workspace(DEFAULT_WORKSPACE).unwrap().path.present(),
            Some(repo.as_path())
        );

        std::fs::remove_dir_all(&repo).unwrap();
    }
}
