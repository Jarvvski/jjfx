//! Repository-owned Worker Workspace lifecycle operations.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;

use crate::{Expected, Loaded, Repository, StateChange, WireStatus, WorkerId, WorkerState};

const CACHE_PATH: &str = ".jj/ws-cache";
const DEFAULT_WORKSPACE: &str = "default";
const SYNAPSE_PATH: &str = "tools/dev-cli/synapse/clone";

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

/// Provisions one Go-compatible Worker Workspace and its idle Worker state.
pub(crate) fn provision(
    repository: &Repository,
    worker_id: &WorkerId,
) -> Result<WorkerWorkspace, WorkerWorkspaceError> {
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
    if let Err(error) = add_workspace(root, worker_id, &path) {
        if !base_existed {
            let _ = fs::remove_dir(base);
        }
        return Err(error);
    }

    let mut state_revision = None;
    let result = (|| {
        copy_setup_sources(root, &path)?;
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

fn worker_path(root: &Path, worker_id: &WorkerId) -> PathBuf {
    let base = env::var_os("JJ_WS_DIR")
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
        });
    base.join(worker_id.as_str())
}

fn add_workspace(
    root: &Path,
    worker_id: &WorkerId,
    path: &Path,
) -> Result<(), WorkerWorkspaceError> {
    let output = Command::new("jj")
        .args(["workspace", "add", "--name", worker_id.as_str()])
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

fn forget_workspace(root: &Path, worker_id: &WorkerId) -> Result<(), WorkerWorkspaceError> {
    let output = Command::new("jj")
        .args(["workspace", "forget", worker_id.as_str()])
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

fn copy_setup_sources(root: &Path, destination: &Path) -> Result<(), WorkerWorkspaceError> {
    let env_source = root.join(".env");
    let env_destination = destination.join(".env");
    if env_source.exists() && !env_destination.exists() {
        fs::copy(&env_source, &env_destination).map_err(|error| {
            WorkerWorkspaceError::new(format!("copy .env into Worker Workspace: {error}"))
        })?;
    }

    let synapse_source = root.join(SYNAPSE_PATH);
    let synapse_destination = destination.join(SYNAPSE_PATH);
    if synapse_source.is_dir() && !synapse_destination.exists() {
        copy_directory(&synapse_source, &synapse_destination).map_err(|error| {
            WorkerWorkspaceError::new(format!("copy Synapse clone into Worker Workspace: {error}"))
        })?;
    }
    Ok(())
}

fn copy_directory(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_name() == ".git" {
            continue;
        }
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_directory(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("unsupported Synapse entry {}", source_path.display()),
            ));
        }
    }
    Ok(())
}

fn project_cache(
    root: &Path,
    worker_id: &WorkerId,
    path: &Path,
) -> Result<(), WorkerWorkspaceError> {
    let cache = cache_path(root);
    let mut entries = read_cache(&cache)?;
    if !entries.iter().any(|(name, _)| name == DEFAULT_WORKSPACE) {
        entries.insert(0, (DEFAULT_WORKSPACE.to_owned(), root.to_owned()));
    }
    entries.push((worker_id.as_str().to_owned(), path.to_path_buf()));
    write_cache(&cache, &entries)
}

fn cache_path(root: &Path) -> PathBuf {
    root.join(CACHE_PATH)
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
    let temporary = parent.join(format!("ws-cache.{}.tmp", std::process::id()));
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
    if let Some(revision) = state_revision
        && let Err(error) = state.commit(Expected::Match(revision), StateChange::Remove)
    {
        failures.push(format!("remove Worker state: {error}"));
    }
    if let Err(error) = forget_workspace(root, worker_id) {
        failures.push(error.to_string());
    }
    if path.exists()
        && let Err(error) = fs::remove_dir_all(path)
    {
        failures.push(format!("remove Worker Workspace directory: {error}"));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(WorkerWorkspaceError::new(failures.join("; ")))
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
