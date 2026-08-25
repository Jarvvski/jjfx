//! Repository-owned Worker Workspace lifecycle operations.

use std::env;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use rustix::fs::{FlockOperation, flock};
use thiserror::Error;

use crate::{Expected, Loaded, Repository, StateChange, WireStatus, WorkerId, WorkerState};

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

/// An error from an Ad Hoc Workspace lifecycle operation.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct AdHocWorkspaceError {
    message: String,
}

impl AdHocWorkspaceError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
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

    /// Reads the ordered Workspace projection, refreshing a missing or stale cache.
    pub fn snapshot(&self) -> Result<WorkspaceSnapshot, AdHocWorkspaceError> {
        let cache = cache_path(self.repository.root());
        if !cache.is_file() || cache_is_stale(self.repository.root(), &cache) {
            return self.refresh();
        }
        let entries = read_cache(&cache)
            .map_err(|error| AdHocWorkspaceError::new(error.message))?
            .into_iter()
            .map(|(name, path)| WorkspaceEntry { name, path })
            .collect();
        Ok(WorkspaceSnapshot { entries })
    }

    /// Rebuilds the compatible Workspace cache from jj's live Workspace list.
    pub fn refresh(&self) -> Result<WorkspaceSnapshot, AdHocWorkspaceError> {
        let names = workspace_names(self.repository.root())
            .map_err(|error| AdHocWorkspaceError::new(error.message))?;
        let mut entries = Vec::with_capacity(names.len() + 1);
        let mut has_default = false;
        for name in names {
            if name == DEFAULT_WORKSPACE {
                has_default = true;
            }
            entries.push(WorkspaceEntry {
                path: self.path(&name),
                name,
            });
        }
        if !has_default {
            entries.insert(
                0,
                WorkspaceEntry {
                    name: DEFAULT_WORKSPACE.to_owned(),
                    path: self.repository.root().to_path_buf(),
                },
            );
        }
        let cache_entries = entries
            .iter()
            .map(|entry| (entry.name.clone(), entry.path.clone()))
            .collect::<Vec<_>>();
        with_cache_lock(self.repository.root(), || {
            write_cache(&cache_path(self.repository.root()), &cache_entries)
        })
        .map_err(|error| AdHocWorkspaceError::new(error.message))?;
        Ok(WorkspaceSnapshot { entries })
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
        add_workspace_with_revision(self.repository.root(), name, &path, revision)
            .map_err(|error| AdHocWorkspaceError::new(error.message))?;
        if let Err(error) = run_setup_hook(&path, on_output) {
            let error = match rollback_workspace_creation(self.repository.root(), name, &path) {
                Ok(()) => error,
                Err(cleanup) => format!("{error}; compensation failed: {cleanup}"),
            };
            return Err(AdHocWorkspaceError::new(error));
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

    let path = ad_hoc_path(root, name);
    add_workspace_with_revision(root, name, &path, revision)
        .map_err(|error| AdHocWorkspaceError::new(error.message))?;
    if let Err(error) = run_setup_hook(&path, on_output) {
        let error = match rollback_workspace_creation(root, name, &path) {
            Ok(()) => error,
            Err(cleanup) => format!("{error}; compensation failed: {cleanup}"),
        };
        return Err(AdHocWorkspaceError::new(error));
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
        .unwrap_or_else(|| ad_hoc_path(root, name));
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

fn ad_hoc_path(root: &Path, name: &str) -> PathBuf {
    let base = root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("repo");
    root.parent().unwrap_or(root).join(format!("{base}-{name}"))
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
    let pool = repository.root().join(".jj/pool");
    fs::create_dir_all(&pool).map_err(|error| {
        WorkerWorkspaceError::new(format!("create Worker operation lock directory: {error}"))
    })?;
    let path = pool.join(format!("{worker}.workspace.lock"));
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| {
            WorkerWorkspaceError::new(format!(
                "open Worker operation lock {}: {error}",
                path.display()
            ))
        })?;
    flock(&lock, FlockOperation::LockExclusive).map_err(|error| {
        WorkerWorkspaceError::new(format!(
            "lock Worker operation sidecar {}: {error}",
            path.display()
        ))
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
    if let Err(error) = add_workspace(root, worker_id.as_str(), &path) {
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
            crate::CommitOutcome::Applied(loaded) => loaded,
            crate::CommitOutcome::Conflict(_) => {
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
    state: &crate::WorkerStateRepository,
    expected: Option<crate::StateRevision<WorkerState>>,
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
    state: &crate::WorkerStateRepository,
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

fn add_workspace(root: &Path, name: &str, path: &Path) -> Result<(), WorkerWorkspaceError> {
    add_workspace_with_revision(root, name, path, None)
}

fn add_workspace_with_revision(
    root: &Path,
    name: &str,
    path: &Path,
    revision: Option<&str>,
) -> Result<(), WorkerWorkspaceError> {
    let mut command = Command::new("jj");
    command.args(["workspace", "add", "--name", name]);
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

fn workspace_names(root: &Path) -> Result<Vec<String>, WorkerWorkspaceError> {
    let output = Command::new("jj")
        .args(["workspace", "list"])
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
        .filter_map(|line| line.split_once(':').map(|(name, _)| name.trim().to_owned()))
        .filter(|name| !name.is_empty())
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
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| WorkerWorkspaceError::new(format!("open ws-cache lock: {error}")))?;
    file.lock()
        .map_err(|error| WorkerWorkspaceError::new(format!("lock ws-cache: {error}")))?;
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
    let parent = path
        .parent()
        .ok_or_else(|| WorkerWorkspaceError::new("write ws-cache: path has no parent"))?;
    fs::create_dir_all(parent).map_err(|error| {
        WorkerWorkspaceError::new(format!("create ws-cache directory: {error}"))
    })?;
    let thread = format!("{:?}", std::thread::current().id());
    let temporary = parent.join(format!("ws-cache.{}.{}.tmp", std::process::id(), thread));
    let mut text = String::new();
    for (name, workspace_path) in entries {
        text.push_str(name);
        text.push('\t');
        text.push_str(&workspace_path.to_string_lossy());
        text.push('\n');
    }
    fs::write(&temporary, text.as_bytes()).map_err(|error| {
        WorkerWorkspaceError::new(format!("write ws-cache temporary file: {error}"))
    })?;
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        WorkerWorkspaceError::new(format!("replace ws-cache: {error}"))
    })?;
    Ok(())
}

fn rollback_provisioning(
    root: &Path,
    worker_id: &WorkerId,
    path: &Path,
    state: &crate::WorkerStateRepository,
    state_revision: Option<crate::StateRevision<WorkerState>>,
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
            Ok(crate::CommitOutcome::Applied(_)) => Ok(()),
            Ok(crate::CommitOutcome::Conflict(_)) => Err(WorkerWorkspaceError::new(
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
