//! Repository-owned Worker Workspace lifecycle operations.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use thiserror::Error;

use crate::engine::atomic::{self, AtomicWriteError, LockError};
use crate::engine::{Expected, Loaded, Repository, StateChange, WireStatus, WorkerId, WorkerState};

const CACHE_PATH: &str = ".jj/ws-cache";
const CACHE_LOCK_PATH: &str = ".jj/ws-cache.lock";
const DEFAULT_WORKSPACE: &str = "default";
const SETUP_HOOK_PATH: &str = ".jjfx/setup.sh";
const TEARDOWN_HOOK_PATH: &str = ".jjfx/teardown.sh";

/// The output stream used by a repository Workspace lifecycle hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceHookStream {
    /// Output written to the hook's standard output.
    Stdout,
    /// Output written to the hook's standard error.
    Stderr,
}

/// A user-created Workspace independent of the Worker Pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdHocWorkspace {
    name: String,
    path: PathBuf,
}

impl AdHocWorkspace {
    /// Returns the normalized jj Workspace name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the sibling path chosen for the Workspace.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Which step of the Ad Hoc Workspace lifecycle an [`AdHocWorkspaceError`] came
/// from, so callers can phrase the failure accurately: nothing was created when
/// the creation step fails, whereas a `Hook` failure means the Workspace exists
/// but its setup did not finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdHocWorkspacePhase {
    /// The Workspace itself could not be created (validation, naming, or
    /// `jj workspace add`).
    Create,
    /// The Workspace was created but its `.jjfx/setup.sh` hook failed.
    Hook,
}

/// An error from an Ad Hoc Workspace lifecycle operation.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct AdHocWorkspaceError {
    message: String,
    phase: AdHocWorkspacePhase,
}

impl AdHocWorkspaceError {
    /// A creation-step failure; nothing was created.
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            phase: AdHocWorkspacePhase::Create,
        }
    }

    /// A setup-hook failure; the Workspace was created but setup did not finish.
    fn hook(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            phase: AdHocWorkspacePhase::Hook,
        }
    }

    /// Which lifecycle step this error came from.
    pub fn phase(&self) -> AdHocWorkspacePhase {
        self.phase
    }
}

/// The frontend's explicit decision for a clean operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanDecision {
    /// Apply the clean plan.
    Confirmed,
    /// Leave every planned Workspace untouched.
    Declined,
}

/// A Workspace entry projected from jj and the compatible cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceEntry {
    name: String,
    path: PathBuf,
    /// Whether `path` was recorded in the compatible cache rather than derived
    /// from the naming convention. A recorded path is authoritative - jjfx or
    /// the shell tools wrote it for this Workspace - so its absence means the
    /// Workspace moved or was removed. A derived path is only a guess, so its
    /// absence means the path is simply not known.
    path_is_recorded: bool,
}

impl WorkspaceEntry {
    /// Returns the stable jj Workspace name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the projected filesystem path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reports whether the projected Workspace directory is absent.
    pub fn is_missing(&self) -> bool {
        !self.path.is_dir()
    }

    /// Reports whether the projected path is recorded in the cache rather than
    /// derived from the naming convention. Callers use this to tell a Workspace
    /// that moved or was removed (a recorded path whose directory is gone) from
    /// one whose path jjfx never knew (a derived path that does not exist).
    pub fn path_is_recorded(&self) -> bool {
        self.path_is_recorded
    }
}

/// An ordered Workspace projection used by CLI adapters and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSnapshot {
    entries: Vec<WorkspaceEntry>,
}

impl WorkspaceSnapshot {
    /// Returns Workspaces in compatible cache order.
    pub fn entries(&self) -> &[WorkspaceEntry] {
        &self.entries
    }
}

/// A Workspace add result that distinguishes default, existing, and created paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceAddOutcome {
    /// The default Workspace was requested.
    Default(PathBuf),
    /// The requested Workspace already existed.
    Existing(AdHocWorkspace),
    /// A new Workspace was created.
    Created(AdHocWorkspace),
}

/// An immutable set of Workspace names selected for cleanup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceCleanPlan {
    entries: Vec<WorkspaceEntry>,
}

impl WorkspaceCleanPlan {
    /// Returns the ordered non-default entries selected for cleanup.
    pub fn entries(&self) -> &[WorkspaceEntry] {
        &self.entries
    }
}

/// Deep Repository-owned Workspace discovery and lifecycle operations.
#[derive(Debug, Clone)]
pub struct Workspaces {
    repository: Repository,
}

impl Workspaces {
    pub(crate) fn new(repository: Repository) -> Self {
        Self { repository }
    }

    /// Returns the compatible base directory for named Workspaces.
    pub fn base_dir(&self) -> PathBuf {
        workspace_base(self.repository.root())
    }

    /// Resolves a Workspace name without requiring it to exist.
    pub fn path(&self, name: &str) -> PathBuf {
        if name == DEFAULT_WORKSPACE {
            self.repository.root().to_path_buf()
        } else {
            self.base_dir().join(name)
        }
    }

    /// Reads the ordered Workspace projection, refreshing a missing or stale
    /// cache.
    ///
    /// The returned projection is the union [`Workspaces::projection`] builds:
    /// jj's live Workspace names, the mirror's recorded paths, and the derived
    /// default. The only difference is that a missing or stale mirror is rebuilt
    /// first. A read never invents a path - the mirror holds the paths jjfx
    /// knows, and a Workspace whose path it never learned is reported as
    /// unknown rather than at a guessed one.
    pub fn snapshot(&self) -> Result<WorkspaceSnapshot, AdHocWorkspaceError> {
        let cache = cache_path(self.repository.root());
        if !cache.is_file() || cache_is_stale(self.repository.root(), &cache) {
            return self.refresh();
        }
        self.projection()
    }

    /// Reads the current Workspace projection without writing the mirror.
    ///
    /// Existence is the union of jj's live Workspace names and the cache's
    /// `name\tpath` entries (ADR 0006): a path already cached for a name is
    /// preserved (jj does not record paths), a jj-only name falls back to the
    /// naming convention, and a cache-only name keeps its cached path. The
    /// default Workspace is always included and pinned to the repository root.
    /// Each entry reports whether its path was recorded or derived
    /// ([`WorkspaceEntry::path_is_recorded`]), so a caller can tell a Workspace
    /// that moved from one whose path was never known.
    /// Use [`Workspaces::refresh`] to rebuild and persist the mirror.
    pub fn projection(&self) -> Result<WorkspaceSnapshot, AdHocWorkspaceError> {
        let names = workspace_names(self.repository.root())
            .map_err(|error| AdHocWorkspaceError::new(error.message))?;
        let cached = read_cache(&cache_path(self.repository.root())).unwrap_or_default();
        let mut entries = Vec::with_capacity(names.len() + cached.len() + 1);
        let mut seen = std::collections::HashSet::new();
        entries.push(WorkspaceEntry {
            name: DEFAULT_WORKSPACE.to_owned(),
            path: self.repository.root().to_path_buf(),
            path_is_recorded: true,
        });
        seen.insert(DEFAULT_WORKSPACE.to_owned());
        for name in names {
            if !seen.insert(name.clone()) {
                continue;
            }
            let (path, path_is_recorded) = cached
                .iter()
                .find(|(cached_name, _)| cached_name == &name)
                .map_or_else(
                    || (self.path(&name), false),
                    |(_, path)| (path.clone(), true),
                );
            entries.push(WorkspaceEntry {
                name,
                path,
                path_is_recorded,
            });
        }
        for (name, path) in cached {
            if seen.insert(name.clone()) {
                entries.push(WorkspaceEntry {
                    name,
                    path,
                    path_is_recorded: true,
                });
            }
        }
        Ok(WorkspaceSnapshot { entries })
    }

    /// Rebuilds the compatible Workspace cache from the live projection.
    ///
    /// The mirror records the paths jjfx knows, so only recorded entries are
    /// written. A path that fell back to the naming convention is a guess, and
    /// persisting it would make the next read treat the guess as a fact - which
    /// is how a Workspace whose real path jjfx never learned ends up recorded at
    /// the wrong one, and then reported as missing there.
    pub fn refresh(&self) -> Result<WorkspaceSnapshot, AdHocWorkspaceError> {
        let snapshot = self.projection()?;
        let cache_entries = snapshot
            .entries
            .iter()
            .filter(|entry| entry.path_is_recorded)
            .map(|entry| (entry.name.clone(), entry.path.clone()))
            .collect::<Vec<_>>();
        with_cache_lock(self.repository.root(), || {
            write_cache(&cache_path(self.repository.root()), &cache_entries)
        })
        .map_err(|error| AdHocWorkspaceError::new(error.message))?;
        Ok(snapshot)
    }

    /// Adds a Workspace, preserving the Go-compatible idempotent behavior.
    pub fn add(
        &self,
        requested_name: &str,
        revision: Option<&str>,
    ) -> Result<WorkspaceAddOutcome, AdHocWorkspaceError> {
        self.add_with_progress(requested_name, revision, |_, _| {})
    }

    /// Adds a Workspace while reporting lifecycle hook output.
    pub fn add_with_progress<F>(
        &self,
        requested_name: &str,
        revision: Option<&str>,
        on_output: F,
    ) -> Result<WorkspaceAddOutcome, AdHocWorkspaceError>
    where
        F: FnMut(WorkspaceHookStream, &str),
    {
        let name = requested_name.trim();
        validate_workspace_name(name)?;
        if name == DEFAULT_WORKSPACE {
            return Ok(WorkspaceAddOutcome::Default(
                self.repository.root().to_path_buf(),
            ));
        }
        let snapshot = self.snapshot()?;
        if let Some(existing) = snapshot.entries.iter().find(|entry| entry.name == name) {
            return Ok(WorkspaceAddOutcome::Existing(AdHocWorkspace {
                name: name.to_owned(),
                path: existing.path.clone(),
            }));
        }
        let path = self.path(name);
        let base = path
            .parent()
            .ok_or_else(|| AdHocWorkspaceError::new("workspace path has no parent"))?;
        fs::create_dir_all(base).map_err(|error| {
            AdHocWorkspaceError::new(format!("create workspace directory: {error}"))
        })?;
        let existed_before = path.exists();
        if let Err(error) = add_workspace_with_revision(
            self.repository.root(),
            name,
            &path,
            revision,
            self.repository.colocate(),
        ) {
            cleanup_failed_create(self.repository.root(), name, &path, existed_before);
            return Err(AdHocWorkspaceError::new(error.message));
        }
        if let Err(error) = run_setup_hook(&path, on_output) {
            let error = match rollback_workspace_creation(self.repository.root(), name, &path) {
                Ok(()) => error,
                Err(cleanup) => format!("{error}; compensation failed: {cleanup}"),
            };
            return Err(AdHocWorkspaceError::hook(error));
        }
        self.refresh()?;
        Ok(WorkspaceAddOutcome::Created(AdHocWorkspace {
            name: name.to_owned(),
            path,
        }))
    }

    /// Removes one named Workspace, optionally skipping the repository teardown hook.
    pub fn remove(&self, name: &str, force: bool) -> Result<bool, AdHocWorkspaceError> {
        self.remove_with_progress(name, force, |_, _| {})
    }

    /// Removes one named Workspace while reporting teardown hook output.
    pub fn remove_with_progress<F>(
        &self,
        name: &str,
        force: bool,
        mut on_output: F,
    ) -> Result<bool, AdHocWorkspaceError>
    where
        F: FnMut(WorkspaceHookStream, &str),
    {
        if name == DEFAULT_WORKSPACE {
            return Err(AdHocWorkspaceError::new(
                "the default workspace cannot be deleted",
            ));
        }
        let root = self.repository.root();
        let path = self.path(name);
        let existed = path.is_dir();
        let mut failures = Vec::new();
        if !force
            && existed
            && let Err(error) = run_teardown_hook(&path, &mut on_output)
        {
            failures.push(error);
        }
        match workspace_names(root) {
            Ok(names) if names.iter().any(|candidate| candidate == name) => {
                if let Err(error) = forget_workspace(root, name) {
                    failures.push(error.message);
                }
            }
            Ok(_) => {}
            Err(error) => failures.push(error.message),
        }
        if existed && let Err(error) = fs::remove_dir_all(&path) {
            failures.push(format!("remove Workspace directory: {error}"));
        }
        if let Err(error) = unproject_cache_entry(root, name) {
            failures.push(error.message);
        }
        if failures.is_empty() {
            Ok(existed)
        } else {
            Err(AdHocWorkspaceError::new(failures.join("; ")))
        }
    }

    /// Plans removal of every non-default Workspace in projection order.
    pub fn plan_clean(&self) -> Result<WorkspaceCleanPlan, AdHocWorkspaceError> {
        let snapshot = self.snapshot()?;
        Ok(WorkspaceCleanPlan {
            entries: snapshot
                .entries
                .into_iter()
                .filter(|entry| entry.name != DEFAULT_WORKSPACE)
                .collect(),
        })
    }

    /// Applies or declines a previously rendered clean plan.
    pub fn clean(
        &self,
        plan: &WorkspaceCleanPlan,
        decision: CleanDecision,
    ) -> Result<(), AdHocWorkspaceError> {
        self.clean_with_progress(plan, decision, |_, _| {})
    }

    /// Applies a clean plan while reporting teardown hook output.
    pub fn clean_with_progress<F>(
        &self,
        plan: &WorkspaceCleanPlan,
        decision: CleanDecision,
        mut on_output: F,
    ) -> Result<(), AdHocWorkspaceError>
    where
        F: FnMut(WorkspaceHookStream, &str),
    {
        if decision == CleanDecision::Declined {
            return Ok(());
        }
        for entry in &plan.entries {
            self.remove_with_progress(&entry.name, false, &mut on_output)?;
        }
        Ok(())
    }
}

fn validate_workspace_name(name: &str) -> Result<(), AdHocWorkspaceError> {
    let path = Path::new(name);
    if name.is_empty()
        || path.components().count() != 1
        || !matches!(
            path.components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return Err(AdHocWorkspaceError::new("workspace name required"));
    }
    Ok(())
}

fn cache_is_stale(root: &Path, cache: &Path) -> bool {
    let operation_heads = root.join(".jj/repo/op_heads/heads");
    match (fs::metadata(cache), fs::metadata(operation_heads)) {
        (Ok(cache), Ok(operation_heads)) => operation_heads
            .modified()
            .ok()
            .zip(cache.modified().ok())
            .is_some_and(|(operation, cached)| operation > cached),
        _ => false,
    }
}

/// Creates an Ad Hoc Workspace without applying Worker Pool policy.
pub(crate) fn create_ad_hoc<F>(
    repository: &Repository,
    requested_name: &str,
    revision: Option<&str>,
    on_output: F,
) -> Result<AdHocWorkspace, AdHocWorkspaceError>
where
    F: FnMut(WorkspaceHookStream, &str),
{
    let name = requested_name.trim();
    if name.is_empty() {
        return Err(AdHocWorkspaceError::new("workspace name required"));
    }
    let root = repository.root();
    let names = workspace_names(root).map_err(|error| AdHocWorkspaceError::new(error.message))?;
    let cache_entries = read_cache(&cache_path(root)).unwrap_or_default();
    if names.iter().any(|candidate| candidate == name)
        || cache_entries.iter().any(|(candidate, _)| candidate == name)
    {
        return Err(AdHocWorkspaceError::new(format!(
            "workspace '{name}' already exists"
        )));
    }

    let path = ad_hoc_workspace_path(root, name);
    let base = path
        .parent()
        .ok_or_else(|| AdHocWorkspaceError::new("workspace path has no parent"))?;
    // jj does not create missing parents, and the shared base directory may not
    // exist yet on a fresh repository.
    fs::create_dir_all(base).map_err(|error| {
        AdHocWorkspaceError::new(format!("create workspace directory: {error}"))
    })?;
    let existed_before = path.exists();
    if let Err(error) =
        add_workspace_with_revision(root, name, &path, revision, repository.colocate())
    {
        cleanup_failed_create(root, name, &path, existed_before);
        return Err(AdHocWorkspaceError::new(error.message));
    }
    if let Err(error) = run_setup_hook(&path, on_output) {
        let error = match rollback_workspace_creation(root, name, &path) {
            Ok(()) => error,
            Err(cleanup) => format!("{error}; compensation failed: {cleanup}"),
        };
        return Err(AdHocWorkspaceError::hook(error));
    }
    let _ = project_cache_entry(root, name, &path);
    Ok(AdHocWorkspace {
        name: name.to_owned(),
        path,
    })
}

/// Removes an Ad Hoc Workspace while protecting the Default Workspace.
pub(crate) fn remove_ad_hoc(
    repository: &Repository,
    name: &str,
    known_path: Option<&Path>,
) -> Result<(), AdHocWorkspaceError> {
    if name == DEFAULT_WORKSPACE {
        return Err(AdHocWorkspaceError::new(
            "the default workspace cannot be deleted",
        ));
    }
    let root = repository.root();
    let path = known_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| ad_hoc_workspace_path(root, name));
    let mut failures = Vec::new();
    if path != root
        && path.is_dir()
        && let Err(error) = run_teardown_hook(&path, |_, _| {})
    {
        failures.push(error);
    }
    if let Err(error) = forget_workspace(root, name) {
        failures.push(error.message);
    }
    if path != root
        && path.is_dir()
        && let Err(error) = fs::remove_dir_all(&path)
    {
        failures.push(format!("remove Workspace directory: {error}"));
    }
    if let Err(error) = unproject_cache_entry(root, name) {
        failures.push(error.message);
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(AdHocWorkspaceError::new(failures.join("; ")))
    }
}

/// The on-disk path for a new ad hoc Workspace before it exists: the shared
/// sibling base directory used by Workers and the CLI, so the TUI's pending
/// tombstone and the CLI's `Workspaces::path` can never disagree. Resolves to
/// `<parent>/<repo>-workspaces/<name>`, or `$JJ_WS_DIR/<name>` when set.
pub fn ad_hoc_workspace_path(root: &Path, name: &str) -> PathBuf {
    workspace_base(root).join(name)
}

/// The provisioned Workspace backing one Worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerWorkspace {
    worker_id: WorkerId,
    path: PathBuf,
}

impl WorkerWorkspace {
    /// Returns the stable Worker identity assigned to this Workspace.
    pub fn worker_id(&self) -> &WorkerId {
        &self.worker_id
    }

    /// Returns the filesystem path of the Worker Workspace.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// An error from the complete Worker Workspace provisioning operation.
#[derive(Debug, Error)]
#[error("Worker Workspace provisioning failed: {message}")]
pub struct WorkerWorkspaceError {
    message: String,
}

impl WorkerWorkspaceError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Exclusive ownership of one Worker's external Workspace operations.
pub(crate) struct WorkspaceOperationGuard {
    _lock: std::fs::File,
}

/// A prepared Workspace whose operation lock remains held for Run handoff.
pub(crate) struct PreparedWorkerWorkspace {
    _workspace: WorkerWorkspace,
    _operation: WorkspaceOperationGuard,
}

pub(crate) fn lock_worker_operation(
    repository: &Repository,
    worker: &WorkerId,
) -> Result<WorkspaceOperationGuard, WorkerWorkspaceError> {
    let path = repository
        .root()
        .join(".jj/pool")
        .join(format!("{worker}.workspace.lock"));
    let lock = atomic::lock_exclusive(&path).map_err(|error| match error {
        LockError::CreateDirectory(source) => {
            WorkerWorkspaceError::new(format!("create Worker operation lock directory: {source}"))
        }
        LockError::Open(source) => WorkerWorkspaceError::new(format!(
            "open Worker operation lock {}: {source}",
            path.display()
        )),
        LockError::Lock(source) => WorkerWorkspaceError::new(format!(
            "lock Worker operation sidecar {}: {source}",
            path.display()
        )),
    })?;
    Ok(WorkspaceOperationGuard { _lock: lock })
}

pub(crate) fn prepare_for_dispatch(
    repository: &Repository,
    worker: &WorkerId,
    base_revisions: &[String],
) -> Result<PreparedWorkerWorkspace, WorkerWorkspaceError> {
    let operation = lock_worker_operation(repository, worker)?;
    let path = worker_path(repository.root(), worker);
    if !path.is_dir() {
        return Err(WorkerWorkspaceError::new(format!(
            "Worker {worker} Workspace is missing at {}",
            path.display()
        )));
    }
    let mut command = Command::new("jj");
    command.arg("new");
    if base_revisions.is_empty() {
        command.arg("main");
    } else {
        command.args(base_revisions);
    }
    let output = command.current_dir(&path).output().map_err(|error| {
        WorkerWorkspaceError::new(format!("start jj new for Worker {worker}: {error}"))
    })?;
    if !output.status.success() {
        return Err(WorkerWorkspaceError::new(format!(
            "prepare Worker {worker} on requested base: {}",
            command_error(&output)
        )));
    }
    Ok(PreparedWorkerWorkspace {
        _workspace: WorkerWorkspace {
            worker_id: worker.clone(),
            path,
        },
        _operation: operation,
    })
}

/// Provisions one Go-compatible Worker Workspace and its idle Worker state.
pub(crate) fn provision(
    repository: &Repository,
    worker_id: &WorkerId,
) -> Result<WorkerWorkspace, WorkerWorkspaceError> {
    provision_with_progress(repository, worker_id, |_, _| {})
}

pub(crate) fn provision_with_progress<F>(
    repository: &Repository,
    worker_id: &WorkerId,
    mut on_output: F,
) -> Result<WorkerWorkspace, WorkerWorkspaceError>
where
    F: FnMut(WorkspaceHookStream, &str),
{
    let root = repository.root();
    let path = worker_path(root, worker_id);
    let state = repository.state_store().worker(worker_id.clone());

    ensure_unclaimed(root, worker_id, &path, &state)?;
    let base = path
        .parent()
        .ok_or_else(|| WorkerWorkspaceError::new("Worker Workspace path has no parent"))?;
    let base_existed = base.exists();
    fs::create_dir_all(base).map_err(|error| {
        WorkerWorkspaceError::new(format!("create Worker Workspace directory: {error}"))
    })?;
    if let Err(error) = add_workspace(root, worker_id.as_str(), &path, repository.colocate()) {
        if !base_existed {
            let _ = fs::remove_dir(base);
        }
        return Err(error);
    }

    let mut state_revision = None;
    let result = (|| {
        run_setup_hook(&path, &mut on_output).map_err(WorkerWorkspaceError::new)?;
        let idle = WorkerState::new(WireStatus::new("idle"));
        let committed = state
            .commit(Expected::Missing, StateChange::Replace(idle))
            .map_err(|error| {
                WorkerWorkspaceError::new(format!("create idle Worker state: {error}"))
            })?;
        let loaded = match committed {
            crate::engine::CommitOutcome::Applied(loaded) => loaded,
            crate::engine::CommitOutcome::Conflict(_) => {
                return Err(WorkerWorkspaceError::new(
                    "create idle Worker state: Worker state was claimed concurrently",
                ));
            }
        };
        let Loaded::Present(versioned) = loaded else {
            return Err(WorkerWorkspaceError::new(
                "create idle Worker state: state was not written",
            ));
        };
        state_revision = Some(versioned.revision().clone());
        project_cache(root, worker_id, &path)
    })();

    match result {
        Ok(()) => Ok(WorkerWorkspace {
            worker_id: worker_id.clone(),
            path,
        }),
        Err(error) => {
            let rollback = rollback_provisioning(root, worker_id, &path, &state, state_revision);
            match rollback {
                Ok(()) => Err(error),
                Err(cleanup) => Err(WorkerWorkspaceError::new(format!(
                    "{error}; compensation failed: {cleanup}"
                ))),
            }
        }
    }
}

/// Removes a provisioned Worker Workspace and all of its compatible state.
///
/// This is crate-private because lifecycle callers must decide when a Worker
/// is no longer owned by a Pool. It is deliberately best-effort across the
/// external Workspace, state, and cache resources so a failed aggregate
/// mutation can report every compensation failure.
pub(crate) fn deprovision(
    repository: &Repository,
    worker_id: &WorkerId,
) -> Result<(), WorkerWorkspaceError> {
    deprovision_with_progress(repository, worker_id, |_, _| {})
}

pub(crate) fn deprovision_with_progress<F>(
    repository: &Repository,
    worker_id: &WorkerId,
    mut on_output: F,
) -> Result<(), WorkerWorkspaceError>
where
    F: FnMut(WorkspaceHookStream, &str),
{
    let root = repository.root();
    let path = worker_path(root, worker_id);
    let state = repository.state_store().worker(worker_id.clone());
    let revision = match state.load() {
        Ok(Loaded::Present(versioned)) => versioned.revision().clone(),
        Ok(Loaded::Missing) => {
            return Err(WorkerWorkspaceError::new(
                "Worker state is absent during compensation; refusing to remove Workspace",
            ));
        }
        Err(error) => {
            return Err(WorkerWorkspaceError::new(format!(
                "load Worker state for compensation: {error}; refusing to remove Workspace"
            )));
        }
    };

    let resources = teardown_workspace_resources(root, worker_id, &path, &mut on_output);
    let state_result = if resources.external_resources_succeeded {
        remove_detached_state(&state, Some(revision))
    } else {
        Err(WorkerWorkspaceError::new(
            "preserve Worker state until Workspace cleanup succeeds",
        ))
    };
    combine_cleanup_results(resources.result, state_result)
}

/// Finishes cleanup for a Worker already detached from Pool membership.
///
/// Worker state remains as the durable cleanup marker until the external jj,
/// directory, and cache operations have succeeded. Repeating this operation is
/// safe after either partial or complete cleanup.
pub(crate) fn teardown_detached_with_progress<F>(
    repository: &Repository,
    worker_id: &WorkerId,
    mut on_output: F,
) -> Result<(), WorkerWorkspaceError>
where
    F: FnMut(WorkspaceHookStream, &str),
{
    let root = repository.root();
    let path = worker_path(root, worker_id);
    let resources = teardown_workspace_resources(root, worker_id, &path, &mut on_output);
    let state = repository.state_store().worker(worker_id.clone());
    let state_result = if resources.external_resources_succeeded {
        remove_detached_state(&state, None)
    } else {
        Err(WorkerWorkspaceError::new(
            "preserve Worker state until Workspace cleanup succeeds",
        ))
    };
    combine_cleanup_results(resources.result, state_result)
}

struct WorkspaceCleanup {
    result: Result<(), WorkerWorkspaceError>,
    external_resources_succeeded: bool,
}

fn teardown_workspace_resources<F>(
    root: &Path,
    worker_id: &WorkerId,
    path: &Path,
    mut on_output: F,
) -> WorkspaceCleanup
where
    F: FnMut(WorkspaceHookStream, &str),
{
    let mut failures = Vec::new();
    let mut external_failures = Vec::new();
    if path.is_dir()
        && let Err(error) = run_teardown_hook(path, &mut on_output)
    {
        failures.push(error);
    }
    match workspace_names(root) {
        Ok(names) if names.iter().any(|name| name == worker_id.as_str()) => {
            if let Err(error) = forget_workspace(root, worker_id.as_str()) {
                let detail = error.to_string();
                failures.push(detail.clone());
                external_failures.push(detail);
            }
        }
        Ok(_) => {}
        Err(error) => {
            let detail = error.to_string();
            failures.push(detail.clone());
            external_failures.push(detail);
        }
    }
    if path.exists()
        && let Err(error) = fs::remove_dir_all(path)
    {
        let detail = format!("remove Worker Workspace directory: {error}");
        failures.push(detail.clone());
        external_failures.push(detail);
    }
    if let Err(error) = unproject_cache(root, worker_id) {
        let detail = error.to_string();
        failures.push(detail.clone());
        external_failures.push(detail);
    }
    let result = if failures.is_empty() {
        Ok(())
    } else {
        Err(WorkerWorkspaceError::new(format!(
            "Workspace cleanup continued: {}",
            failures.join("; ")
        )))
    };
    WorkspaceCleanup {
        result,
        external_resources_succeeded: external_failures.is_empty(),
    }
}

fn combine_cleanup_results(
    resources: Result<(), WorkerWorkspaceError>,
    state: Result<(), WorkerWorkspaceError>,
) -> Result<(), WorkerWorkspaceError> {
    let mut failures = Vec::new();
    if let Err(error) = resources {
        failures.push(error.message);
    }
    if let Err(error) = state {
        failures.push(error.message);
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(WorkerWorkspaceError::new(format!(
            "Workspace cleanup continued: {}",
            failures.join("; ")
        )))
    }
}

fn remove_detached_state(
    state: &crate::engine::WorkerStateRepository,
    expected: Option<crate::engine::StateRevision<WorkerState>>,
) -> Result<(), WorkerWorkspaceError> {
    match state.remove_detached(expected) {
        Ok(true) => Ok(()),
        Ok(false) => Err(WorkerWorkspaceError::new(
            "Worker state changed during cleanup after Workspace cleanup",
        )),
        Err(error) => Err(WorkerWorkspaceError::new(format!(
            "remove Worker state and log after Workspace cleanup: {error}"
        ))),
    }
}

fn unproject_cache(root: &Path, worker_id: &WorkerId) -> Result<(), WorkerWorkspaceError> {
    unproject_cache_entry(root, worker_id.as_str())
}

fn unproject_cache_entry(root: &Path, workspace_name: &str) -> Result<(), WorkerWorkspaceError> {
    with_cache_lock(root, || {
        let cache = cache_path(root);
        let entries = read_cache(&cache)?;
        let original_len = entries.len();
        let filtered: Vec<_> = entries
            .into_iter()
            .filter(|(name, _)| name != workspace_name)
            .collect();
        if filtered.len() != original_len {
            write_cache(&cache, &filtered)?;
        }
        Ok(())
    })
}

fn ensure_unclaimed(
    root: &Path,
    worker_id: &WorkerId,
    path: &Path,
    state: &crate::engine::WorkerStateRepository,
) -> Result<(), WorkerWorkspaceError> {
    if path.exists() {
        return Err(WorkerWorkspaceError::new(format!(
            "Worker Workspace path already exists: {}",
            path.display()
        )));
    }
    match state
        .load()
        .map_err(|error| WorkerWorkspaceError::new(format!("check Worker state: {error}")))?
    {
        Loaded::Missing => {}
        Loaded::Present(_) => {
            return Err(WorkerWorkspaceError::new(format!(
                "Worker state already exists for {worker_id}"
            )));
        }
    }
    if workspace_names(root)?
        .iter()
        .any(|name| name == worker_id.as_str())
    {
        return Err(WorkerWorkspaceError::new(format!(
            "jj Workspace already exists for {worker_id}"
        )));
    }
    let cache = cache_path(root);
    let entries = read_cache(&cache)?;
    if entries.iter().any(|(name, _)| name == worker_id.as_str()) {
        return Err(WorkerWorkspaceError::new(format!(
            "ws-cache already contains Workspace {worker_id}"
        )));
    }
    Ok(())
}

fn workspace_base(root: &Path) -> PathBuf {
    env::var_os("JJ_WS_DIR")
        .map(PathBuf::from)
        .map(|base| {
            if base.is_absolute() {
                base
            } else {
                root.join(base)
            }
        })
        .unwrap_or_else(|| {
            let name = root
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("repo");
            root.parent()
                .unwrap_or(root)
                .join(format!("{name}-workspaces"))
        })
}

pub(crate) fn worker_path(root: &Path, worker_id: &WorkerId) -> PathBuf {
    workspace_base(root).join(worker_id.as_str())
}

fn add_workspace(
    root: &Path,
    name: &str,
    path: &Path,
    colocate: bool,
) -> Result<(), WorkerWorkspaceError> {
    add_workspace_with_revision(root, name, path, None, colocate)
}

/// The jj major/minor version jjfx is driving, probed from `jj --version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct JjVersion {
    major: u64,
    minor: u64,
}

impl JjVersion {
    /// Parses the `major.minor` prefix of `jj --version` output (`jj 0.46.0`).
    fn parse(output: &str) -> Option<Self> {
        let version = output.split_whitespace().find(|token| {
            token
                .split('.')
                .next()
                .is_some_and(|major| major.chars().all(|c| c.is_ascii_digit()))
        })?;
        let mut parts = version.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().unwrap_or("0").parse().ok()?;
        Some(Self { major, minor })
    }

    /// Whether this jj understands `jj workspace add --colocate`/`--no-colocate`,
    /// added in 0.46.
    fn supports_colocate(self) -> bool {
        (self.major, self.minor) >= (0, 46)
    }
}

impl std::fmt::Display for JjVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)
    }
}

/// The jj major/minor version `jj-lib` targets - the on-disk store format it
/// reads (ADR 0007). Kept equal to `Cargo.toml`'s `jj-lib` pin by
/// `jj_lib_target_tracks_the_cargo_pin`, so it cannot silently drift from the
/// dependency it describes.
const JJ_LIB_TARGET: JjVersion = JjVersion {
    major: 0,
    minor: 46,
};

/// A warning when the `jj` on `PATH` does not match [`JJ_LIB_TARGET`], the
/// version whose store format jj-lib reads. `None` on a match, and `None` when
/// `jj` cannot be probed at all (a missing or broken jj surfaces its own error
/// on the CLI paths that need it, so it is not a mismatch to report here).
///
/// Public because the shared CLI router (also compiled into the `wsg`
/// compatibility binary) resolves the engine through `crate::engine`.
pub fn jj_lib_compat_notice() -> Option<String> {
    compat_notice(jj_version())
}

/// The pure comparison behind [`jj_lib_compat_notice`]: `Some(message)` only
/// when a probed version differs from the jj-lib target.
fn compat_notice(found: Option<JjVersion>) -> Option<String> {
    let found = found?;
    (found != JJ_LIB_TARGET).then(|| {
        format!(
            "jj {found} on PATH differs from the jj-lib target {JJ_LIB_TARGET}; \
             jjfx reads the store with jj-lib and may misread it - install jj {JJ_LIB_TARGET}"
        )
    })
}

/// The jj version jjfx is driving, probed once per process. `None` when `jj
/// --version` cannot be read; callers then assume the current flag behavior so
/// a missing or broken jj surfaces its own error rather than a version guess.
fn jj_version() -> Option<JjVersion> {
    static VERSION: OnceLock<Option<JjVersion>> = OnceLock::new();
    *VERSION.get_or_init(|| {
        let output = Command::new("jj").arg("--version").output().ok()?;
        if !output.status.success() {
            return None;
        }
        JjVersion::parse(&String::from_utf8_lossy(&output.stdout))
    })
}

/// The `jj workspace add` colocation flag, or `None` to omit it. jj added
/// `--colocate`/`--no-colocate` in 0.46: on older jj, omitting the flag is the
/// pre-0.46 behavior (workspaces were never colocated), so non-colocated
/// repositories keep working, while requesting colocation fails with a clear
/// version error instead of jj's raw `unexpected argument '--colocate'`.
///
/// Colocation is requested only when jjfx asks for it *and* the repository
/// itself is colocated. A repository that is not colocated has no Git directory
/// to share, so it never colocates - which is what keeps jjfx's own
/// non-colocated test repositories from creating worktrees.
fn colocation_flag(
    root: &Path,
    colocate: bool,
) -> Result<Option<&'static str>, WorkerWorkspaceError> {
    let version = jj_version();
    let supports = version.is_none_or(JjVersion::supports_colocate);
    if colocate && root.join(".git").is_dir() {
        if supports {
            Ok(Some("--colocate"))
        } else {
            let found = version.map_or_else(|| "unknown".to_owned(), |v| v.to_string());
            Err(WorkerWorkspaceError::new(format!(
                "colocation requires jj >= 0.46 (found {found}); upgrade jj or set colocate = false"
            )))
        }
    } else if supports {
        Ok(Some("--no-colocate"))
    } else {
        Ok(None)
    }
}

fn add_workspace_with_revision(
    root: &Path,
    name: &str,
    path: &Path,
    revision: Option<&str>,
    colocate: bool,
) -> Result<(), WorkerWorkspaceError> {
    let mut command = Command::new("jj");
    command.args(["workspace", "add", "--name", name]);
    if let Some(flag) = colocation_flag(root, colocate)? {
        command.arg(flag);
    }
    if let Some(revision) = revision.filter(|revision| !revision.is_empty()) {
        command.args(["--revision", revision]);
    }
    let output = command
        .arg(path)
        .current_dir(root)
        .output()
        .map_err(|error| WorkerWorkspaceError::new(format!("run jj workspace add: {error}")))?;
    if !output.status.success() {
        return Err(WorkerWorkspaceError::new(format!(
            "run jj workspace add: {}",
            command_error(&output)
        )));
    }
    Ok(())
}

/// Forget one Workspace through jj. When the Workspace is colocated, jj removes
/// its Git worktree registration as part of forgetting it, so teardown needs no
/// separate Git step. (`jj workspace remove` is deliberately not used: it
/// snapshots the working copy into a commit first, which is wrong for a delete.)
fn forget_workspace(root: &Path, name: &str) -> Result<(), WorkerWorkspaceError> {
    let output = Command::new("jj")
        .args(["workspace", "forget", name])
        .current_dir(root)
        .output()
        .map_err(|error| WorkerWorkspaceError::new(format!("run jj workspace forget: {error}")))?;
    if !output.status.success() {
        return Err(WorkerWorkspaceError::new(format!(
            "run jj workspace forget: {}",
            command_error(&output)
        )));
    }
    Ok(())
}

/// Read the repository's git remotes.
///
/// A pure read, so it must not snapshot the working copy: a snapshot churns
/// commits and pings the TUI's watcher (ADR 0006). `jj::read_at_repo` is the
/// same rule; this is the engine's own copy of that read.
pub(crate) fn git_remotes(root: &Path) -> std::io::Result<std::process::Output> {
    Command::new("jj")
        .arg("--ignore-working-copy")
        .args(["git", "remote", "list"])
        .current_dir(root)
        .output()
}

/// List workspace names through an explicit template. The default
/// `jj workspace list` output is version-coupled (0.44 added workspace roots,
/// 0.46 added template labels and unreachable paths), so read the `name` field
/// directly rather than splitting the human-readable line. `name` is a symbol,
/// so it is `stringify`d: jj renders a name needing quoting (`my work`) with its
/// quotes, and every caller here compares against the raw name.
fn workspace_names(root: &Path) -> Result<Vec<String>, WorkerWorkspaceError> {
    let output = Command::new("jj")
        // A pure read. Without this, jj snapshots the working copy and writes
        // under `.jj`, which fires the TUI's recursive `.jj` watcher and
        // re-triggers this projection - a self-sustaining reload loop that
        // storms `jj` and everything downstream of a reload. `jj::read_at_repo`
        // is the same rule; the projection must never ping its own watcher.
        .arg("--ignore-working-copy")
        .args(["workspace", "list", "-T", "stringify(name) ++ \"\\n\""])
        .current_dir(root)
        .output()
        .map_err(|error| WorkerWorkspaceError::new(format!("run jj workspace list: {error}")))?;
    if !output.status.success() {
        return Err(WorkerWorkspaceError::new(format!(
            "run jj workspace list: {}",
            command_error(&output)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect())
}

fn run_setup_hook<F>(workspace: &Path, on_output: F) -> Result<(), String>
where
    F: FnMut(WorkspaceHookStream, &str),
{
    run_hook(workspace, SETUP_HOOK_PATH, on_output)
}

fn run_teardown_hook(
    workspace: &Path,
    on_output: impl FnMut(WorkspaceHookStream, &str),
) -> Result<(), String> {
    run_hook(workspace, TEARDOWN_HOOK_PATH, on_output)
}

fn run_hook<F>(workspace: &Path, relative_path: &str, mut on_output: F) -> Result<(), String>
where
    F: FnMut(WorkspaceHookStream, &str),
{
    let hook = workspace.join(relative_path);
    if !hook.is_file() {
        return Ok(());
    }

    let mut child = Command::new("/bin/sh")
        .arg(relative_path)
        .current_dir(workspace)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("run {relative_path}: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("run {relative_path}: capture stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("run {relative_path}: capture stderr"))?;
    let (tx, rx) = std::sync::mpsc::channel();
    let stdout_thread = spawn_hook_output_reader(stdout, WorkspaceHookStream::Stdout, tx.clone());
    let stderr_thread = spawn_hook_output_reader(stderr, WorkspaceHookStream::Stderr, tx);

    let mut stdout_text = String::new();
    let mut stderr_text = String::new();
    for (stream, line) in rx {
        match stream {
            WorkspaceHookStream::Stdout => stdout_text.push_str(&line),
            WorkspaceHookStream::Stderr => stderr_text.push_str(&line),
        }
        on_output(stream, line.trim_end_matches(['\r', '\n']));
    }

    let mut reader_failures = Vec::new();
    match stdout_thread.join() {
        Ok(Ok(())) => {}
        Ok(Err(error)) => reader_failures.push(format!("read stdout: {error}")),
        Err(_) => reader_failures.push("read stdout: reader thread panicked".to_owned()),
    }
    match stderr_thread.join() {
        Ok(Ok(())) => {}
        Ok(Err(error)) => reader_failures.push(format!("read stderr: {error}")),
        Err(_) => reader_failures.push("read stderr: reader thread panicked".to_owned()),
    }
    if !reader_failures.is_empty() {
        let _ = child.wait();
        return Err(format!(
            "run {relative_path}: {}",
            reader_failures.join("; ")
        ));
    }

    let status = child
        .wait()
        .map_err(|error| format!("wait for {relative_path}: {error}"))?;
    if status.success() {
        return Ok(());
    }
    let detail = stderr_text.trim();
    if detail.is_empty() {
        Err(format!("run {relative_path}: command exited with {status}"))
    } else {
        Err(format!(
            "run {relative_path}: command exited with {status}: {detail}"
        ))
    }
}

fn spawn_hook_output_reader<R: io::Read + Send + 'static>(
    reader: R,
    stream: WorkspaceHookStream,
    tx: std::sync::mpsc::Sender<(WorkspaceHookStream, String)>,
) -> std::thread::JoinHandle<io::Result<()>> {
    std::thread::spawn(move || {
        let mut reader = io::BufReader::new(reader);
        let mut line = Vec::new();
        loop {
            line.clear();
            let bytes = io::BufRead::read_until(&mut reader, b'\n', &mut line)?;
            if bytes == 0 {
                break;
            }
            let text = String::from_utf8_lossy(&line).into_owned();
            tx.send((stream, text)).map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "hook output receiver closed")
            })?;
        }
        Ok(())
    })
}

/// Best-effort removal of a Workspace directory a failed `jj workspace add`
/// left behind. Only removes a directory we just created (`!existed_before`),
/// that is not the repository root, and that jj does not list as a Workspace -
/// so a race where the name appears between the pre-check and the add never
/// destroys someone else's Workspace. A `workspace_names` failure is treated as
/// "still listed" and leaves the directory alone.
fn cleanup_failed_create(root: &Path, name: &str, path: &Path, existed_before: bool) {
    let still_listed = workspace_names(root)
        .map(|names| names.iter().any(|candidate| candidate == name))
        .unwrap_or(true);
    if !should_cleanup_failed_create(existed_before, path == root, path.exists(), still_listed) {
        return;
    }
    let _ = fs::remove_dir_all(path);
    let _ = unproject_cache_entry(root, name);
}

/// The safety guard for [`cleanup_failed_create`]: remove a directory only when
/// we just created it, it is not the repository root, it exists, and jj does not
/// list the name (so a race that registered a Workspace is never destroyed).
fn should_cleanup_failed_create(
    existed_before: bool,
    is_root: bool,
    path_exists: bool,
    still_listed: bool,
) -> bool {
    !existed_before && !is_root && path_exists && !still_listed
}

fn rollback_workspace_creation(root: &Path, name: &str, path: &Path) -> Result<(), String> {
    let mut failures = Vec::new();
    if path.is_dir()
        && let Err(error) = run_teardown_hook(path, |_, _| {})
    {
        failures.push(error);
    }
    match workspace_names(root) {
        Ok(names) if names.iter().any(|candidate| candidate == name) => {
            if let Err(error) = forget_workspace(root, name) {
                failures.push(error.message);
            }
        }
        Ok(_) => {}
        Err(error) => failures.push(error.message),
    }
    if path.exists()
        && let Err(error) = fs::remove_dir_all(path)
    {
        failures.push(format!("remove Workspace directory: {error}"));
    }
    if let Err(error) = unproject_cache_entry(root, name) {
        failures.push(error.message);
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn project_cache(
    root: &Path,
    worker_id: &WorkerId,
    path: &Path,
) -> Result<(), WorkerWorkspaceError> {
    project_cache_entry(root, worker_id.as_str(), path)
}

fn project_cache_entry(
    root: &Path,
    workspace_name: &str,
    path: &Path,
) -> Result<(), WorkerWorkspaceError> {
    with_cache_lock(root, || {
        let cache = cache_path(root);
        let mut entries = read_cache(&cache)?;
        if !entries.iter().any(|(name, _)| name == DEFAULT_WORKSPACE) {
            entries.insert(0, (DEFAULT_WORKSPACE.to_owned(), root.to_owned()));
        }
        if !entries.iter().any(|(name, _)| name == workspace_name) {
            entries.push((workspace_name.to_owned(), path.to_path_buf()));
        }
        write_cache(&cache, &entries)
    })
}

fn cache_path(root: &Path) -> PathBuf {
    root.join(CACHE_PATH)
}

fn with_cache_lock<T>(
    root: &Path,
    operation: impl FnOnce() -> Result<T, WorkerWorkspaceError>,
) -> Result<T, WorkerWorkspaceError> {
    let path = root.join(CACHE_LOCK_PATH);
    let _lock = atomic::lock_exclusive(&path).map_err(|error| match error {
        LockError::CreateDirectory(source) => {
            WorkerWorkspaceError::new(format!("create ws-cache lock directory: {source}"))
        }
        LockError::Open(source) => {
            WorkerWorkspaceError::new(format!("open ws-cache lock: {source}"))
        }
        LockError::Lock(source) => WorkerWorkspaceError::new(format!("lock ws-cache: {source}")),
    })?;
    operation()
}

fn read_cache(path: &Path) -> Result<Vec<(String, PathBuf)>, WorkerWorkspaceError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(WorkerWorkspaceError::new(format!("read ws-cache: {error}")));
        }
    };
    Ok(contents
        .lines()
        .filter_map(|line| {
            let (name, path) = line.split_once('\t')?;
            if name.is_empty() || path.is_empty() {
                None
            } else {
                Some((name.to_owned(), PathBuf::from(path)))
            }
        })
        .collect())
}

fn write_cache(path: &Path, entries: &[(String, PathBuf)]) -> Result<(), WorkerWorkspaceError> {
    let mut text = String::new();
    for (name, workspace_path) in entries {
        text.push_str(name);
        text.push('\t');
        text.push_str(&workspace_path.to_string_lossy());
        text.push('\n');
    }
    atomic::write_atomic_bytes(path, text.as_bytes()).map_err(|error| match error {
        AtomicWriteError::MissingParent => {
            WorkerWorkspaceError::new("write ws-cache: path has no parent")
        }
        AtomicWriteError::CreateDirectory(source) => {
            WorkerWorkspaceError::new(format!("create ws-cache directory: {source}"))
        }
        AtomicWriteError::CreateTemporary(source)
        | AtomicWriteError::SetPermissions(source)
        | AtomicWriteError::Write(source)
        | AtomicWriteError::Flush(source)
        | AtomicWriteError::Sync(source) => {
            WorkerWorkspaceError::new(format!("write ws-cache temporary file: {source}"))
        }
        AtomicWriteError::Rename(source) => {
            WorkerWorkspaceError::new(format!("replace ws-cache: {source}"))
        }
    })
}

fn rollback_provisioning(
    root: &Path,
    worker_id: &WorkerId,
    path: &Path,
    state: &crate::engine::WorkerStateRepository,
    state_revision: Option<crate::engine::StateRevision<WorkerState>>,
) -> Result<(), WorkerWorkspaceError> {
    let mut failures = Vec::new();
    if path.is_dir()
        && let Err(error) = run_teardown_hook(path, |_, _| {})
    {
        failures.push(error);
    }
    if let Err(error) = forget_workspace(root, worker_id.as_str()) {
        failures.push(error.to_string());
    }
    if path.exists()
        && let Err(error) = fs::remove_dir_all(path)
    {
        failures.push(format!("remove Worker Workspace directory: {error}"));
    }
    if let Err(error) = unproject_cache(root, worker_id) {
        failures.push(error.to_string());
    }
    if !failures.is_empty() {
        return Err(WorkerWorkspaceError::new(failures.join("; ")));
    }

    if let Some(revision) = state_revision {
        match state.commit(Expected::Match(revision), StateChange::Remove) {
            Ok(crate::engine::CommitOutcome::Applied(_)) => Ok(()),
            Ok(crate::engine::CommitOutcome::Conflict(_)) => Err(WorkerWorkspaceError::new(
                "Worker state changed during rollback after Workspace cleanup",
            )),
            Err(error) => Err(WorkerWorkspaceError::new(format!(
                "remove Worker state after Workspace cleanup: {error}"
            ))),
        }
    } else {
        Ok(())
    }
}

fn command_error(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.is_empty() {
        format!("command exited with {}", output.status)
    } else {
        stderr
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_repo(tag: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let repo =
            std::env::temp_dir().join(format!("jjfx-engine-{tag}-{}-{nonce}", std::process::id()));
        let status = Command::new("jj")
            .args([
                "--config",
                "signing.behavior=drop",
                "git",
                "init",
                "--no-colocate",
            ])
            .arg(&repo)
            .status()
            .unwrap();
        assert!(status.success(), "jj git init should succeed");
        repo
    }

    #[test]
    fn jj_version_parses_major_and_minor() {
        assert_eq!(
            JjVersion::parse("jj 0.46.0\n"),
            Some(JjVersion {
                major: 0,
                minor: 46
            })
        );
        assert_eq!(
            JjVersion::parse("jj 0.45.1"),
            Some(JjVersion {
                major: 0,
                minor: 45
            })
        );
        assert_eq!(
            JjVersion::parse("jj 1.2.3-dev"),
            Some(JjVersion { major: 1, minor: 2 })
        );
        assert_eq!(JjVersion::parse("nonsense"), None);
    }

    #[test]
    fn jj_lib_compat_notice_fires_only_on_a_mismatch() {
        // A match, and a jj that cannot be probed, are both silent.
        assert_eq!(compat_notice(Some(JJ_LIB_TARGET)), None);
        assert_eq!(compat_notice(None), None);

        let mismatched = JjVersion {
            major: JJ_LIB_TARGET.major,
            minor: JJ_LIB_TARGET.minor + 1,
        };
        let notice = compat_notice(Some(mismatched)).expect("a mismatch must warn");
        assert!(notice.contains(&mismatched.to_string()), "{notice}");
        assert!(notice.contains(&JJ_LIB_TARGET.to_string()), "{notice}");
    }

    /// `JJ_LIB_TARGET` must track `Cargo.toml`'s `jj-lib` pin, or a dependency
    /// bump would leave the compat check comparing against a stale version with
    /// nothing to catch it. Reading the manifest here (rather than a build
    /// script) keeps the check dependency-free while still failing on drift.
    #[test]
    fn jj_lib_target_tracks_the_cargo_pin() {
        let manifest: toml::Value =
            toml::from_str(include_str!("../../Cargo.toml")).expect("Cargo.toml is valid TOML");
        let pin = manifest["dependencies"]["jj-lib"]
            .as_str()
            .expect("Cargo.toml pins jj-lib");
        assert_eq!(
            JjVersion::parse(pin).expect("the jj-lib pin is a version"),
            JJ_LIB_TARGET,
            "JJ_LIB_TARGET must track the jj-lib pin in Cargo.toml"
        );
    }

    #[test]
    fn colocation_flag_gates_on_the_repository_being_colocated() {
        let root = std::env::temp_dir().join(format!(
            "jjfx-engine-colocation-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        // The suite runs against the pinned jj (>= 0.46), so the flag pair is
        // available; the version-dependent branches are covered by the pure
        // `supports_colocate` assertions below.
        assert!(
            !JjVersion {
                major: 0,
                minor: 45
            }
            .supports_colocate()
        );
        assert!(
            JjVersion {
                major: 0,
                minor: 46
            }
            .supports_colocate()
        );
        assert!(
            JjVersion {
                major: 0,
                minor: 47
            }
            .supports_colocate()
        );

        // Without a `.git`, the repository is not colocated, so jjfx never
        // colocates - even when the setting asks for it.
        assert_eq!(colocation_flag(&root, true).unwrap(), Some("--no-colocate"));

        // A `.git` directory marks the repository colocated.
        std::fs::create_dir(root.join(".git")).unwrap();
        assert_eq!(colocation_flag(&root, true).unwrap(), Some("--colocate"));
        // Asking not to colocate always wins.
        assert_eq!(
            colocation_flag(&root, false).unwrap(),
            Some("--no-colocate")
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    fn sibling(root: &Path, name: &str) -> PathBuf {
        let base = root.file_name().unwrap().to_string_lossy();
        root.parent().unwrap().join(format!("{base}-{name}"))
    }

    #[test]
    fn should_cleanup_only_removes_a_freshly_created_unlisted_dir() {
        assert!(should_cleanup_failed_create(false, false, true, false));
        // Left alone when it pre-existed, is the root, is gone, or is still listed.
        assert!(!should_cleanup_failed_create(true, false, true, false));
        assert!(!should_cleanup_failed_create(false, true, true, false));
        assert!(!should_cleanup_failed_create(false, false, false, false));
        assert!(!should_cleanup_failed_create(false, false, true, true));
    }

    #[test]
    fn cleanup_failed_create_removes_a_partial_dir_and_preserves_a_preexisting_one() {
        let repo = temp_repo("cleanup");
        let root = repo.canonicalize().unwrap();
        let path = sibling(&root, "feat");

        // jj created the directory but did not register the Workspace: removed.
        fs::create_dir_all(path.join(".jj")).unwrap();
        cleanup_failed_create(&root, "feat", &path, false);
        assert!(
            !path.exists(),
            "partial Workspace directory should be removed"
        );

        // A directory that existed before the add is never destroyed.
        fs::create_dir_all(&path).unwrap();
        cleanup_failed_create(&root, "feat", &path, true);
        assert!(path.exists(), "pre-existing directory must be preserved");

        std::fs::remove_dir_all(&path).ok();
        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn workspace_names_returns_raw_names_for_names_needing_quoting() {
        // `name` in the template is a symbol, so jj would render `my work` with
        // its quotes; the facade compares against the raw name.
        let repo = temp_repo("raw-names");
        let root = repo.canonicalize().unwrap();
        let path = sibling(&root, "my work");
        add_workspace_with_revision(&root, "my work", &path, None, false).unwrap();

        let names = workspace_names(&root).unwrap();
        assert!(
            names.iter().any(|name| name == "my work"),
            "workspace_names must return the raw name, got {names:?}"
        );

        std::fs::remove_dir_all(&path).ok();
        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn workspace_names_does_not_snapshot_the_working_copy() {
        // `jj workspace list` snapshots the working copy by default, and that
        // snapshot writes under `.jj` - exactly what the TUI's recursive `.jj`
        // watcher reacts to. A projection that snapshots therefore re-triggers
        // its own reload, forever: the storm in ticket 41. The read must be
        // pure, so a dirty working copy is left dirty.
        let repo = temp_repo("pure-read");
        let root = repo.canonicalize().unwrap();
        std::fs::write(root.join("dirty.txt"), "unsnapshotted\n").unwrap();

        let before = working_copy_commit(&root);
        let _ = workspace_names(&root).unwrap();
        let after = working_copy_commit(&root);

        assert_eq!(
            before, after,
            "workspace_names must not snapshot the working copy"
        );

        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn git_remotes_does_not_snapshot_the_working_copy() {
        // `jj git remote list` snapshots the working copy by default, so a
        // dispatch identity check would commit the working copy as a side
        // effect and ping the TUI's watcher (ADR 0006). The read must be pure.
        let repo = temp_repo("pure-remotes");
        let root = repo.canonicalize().unwrap();
        std::fs::write(root.join("dirty.txt"), "unsnapshotted\n").unwrap();

        let before = working_copy_commit(&root);
        let _ = git_remotes(&root);
        let after = working_copy_commit(&root);

        assert_eq!(
            before, after,
            "git_remotes must not snapshot the working copy"
        );

        std::fs::remove_dir_all(&repo).ok();
    }

    /// The working-copy commit id, read without snapshotting it first.
    fn working_copy_commit(root: &Path) -> String {
        let output = Command::new("jj")
            .args([
                "--ignore-working-copy",
                "log",
                "-r",
                "@",
                "--no-graph",
                "-T",
                "commit_id",
            ])
            .current_dir(root)
            .output()
            .expect("jj log");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }
}
