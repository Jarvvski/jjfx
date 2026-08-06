//! The Workspace Dispatch command/event seam used by the jjfx App.
//!
//! The App submits a small command vocabulary and receives immutable events.
//! Adapters own repository access, locks, blocking work, and error mapping.

use std::path::PathBuf;
use std::sync::Arc;

use wsg_core::{PoolCapacity, WorkerPoolSnapshot};

/// A user-visible operation identity used to ignore stale results.
pub type OperationId = u64;

/// Commands accepted by the Workspace Dispatch controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceDispatchCommand {
    /// Reconcile recorded Runs and read a complete immutable Pool snapshot.
    Refresh { operation: OperationId },
    /// Set the exact Pool capacity, creating the Pool when necessary.
    Resize {
        operation: OperationId,
        capacity: usize,
    },
    /// Destroy the Pool after the caller has obtained confirmation.
    Destroy { operation: OperationId },
}

/// The result of a Pool membership mutation, reduced to presentation data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolMutationResult {
    capacity: usize,
    added_workers: Vec<String>,
    removed_workers: Vec<String>,
}

impl PoolMutationResult {
    /// Returns the resulting Pool capacity.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns Workers created by the mutation.
    pub fn added_workers(&self) -> &[String] {
        &self.added_workers
    }

    /// Returns Workers removed by the mutation.
    pub fn removed_workers(&self) -> &[String] {
        &self.removed_workers
    }
}

/// Events emitted after a command completes outside the App event loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceDispatchEvent {
    /// The newest immutable Pool presentation snapshot.
    Snapshot {
        operation: OperationId,
        snapshot: WorkerPoolSnapshot,
    },
    /// A Pool mutation completed and its state is being refreshed.
    Resized {
        operation: OperationId,
        result: PoolMutationResult,
    },
    /// Pool destruction completed.
    Destroyed { operation: OperationId },
    /// A command or its post-mutation refresh failed.
    Failed {
        operation: OperationId,
        message: String,
    },
}

/// The blocking implementation behind the controller seam.
pub trait WorkspaceDispatchAdapter: Send + Sync + 'static {
    /// Reconcile and read the current immutable Pool presentation.
    fn refresh(&self) -> Result<WorkerPoolSnapshot, String>;
    /// Set exact Pool capacity.
    fn resize(&self, capacity: usize) -> Result<PoolMutationResult, String>;
    /// Destroy every Pool Worker and its compatible state.
    fn destroy(&self) -> Result<(), String>;
}

/// A deep asynchronous controller for Workspace Dispatch operations.
#[derive(Clone)]
pub struct WorkspaceDispatchController {
    adapter: Arc<dyn WorkspaceDispatchAdapter>,
    emit: Arc<dyn Fn(WorkspaceDispatchEvent) + Send + Sync>,
}

impl WorkspaceDispatchController {
    /// Creates a controller from an adapter and an event sink.
    pub fn new<A, E>(adapter: A, emit: E) -> Self
    where
        A: WorkspaceDispatchAdapter,
        E: Fn(WorkspaceDispatchEvent) + Send + Sync + 'static,
    {
        Self {
            adapter: Arc::new(adapter),
            emit: Arc::new(emit),
        }
    }

    /// Submits a blocking command and returns immediately.
    pub fn submit(&self, command: WorkspaceDispatchCommand) {
        let adapter = Arc::clone(&self.adapter);
        let emit = Arc::clone(&self.emit);
        std::thread::spawn(move || match command {
            WorkspaceDispatchCommand::Refresh { operation } => match adapter.refresh() {
                Ok(snapshot) => emit(WorkspaceDispatchEvent::Snapshot {
                    operation,
                    snapshot,
                }),
                Err(message) => emit(WorkspaceDispatchEvent::Failed { operation, message }),
            },
            WorkspaceDispatchCommand::Resize {
                operation,
                capacity,
            } => match adapter.resize(capacity) {
                Ok(result) => {
                    emit(WorkspaceDispatchEvent::Resized { operation, result });
                    match adapter.refresh() {
                        Ok(snapshot) => emit(WorkspaceDispatchEvent::Snapshot {
                            operation,
                            snapshot,
                        }),
                        Err(message) => {
                            emit(WorkspaceDispatchEvent::Failed { operation, message });
                        }
                    }
                }
                Err(message) => emit(WorkspaceDispatchEvent::Failed { operation, message }),
            },
            WorkspaceDispatchCommand::Destroy { operation } => match adapter.destroy() {
                Ok(()) => {
                    emit(WorkspaceDispatchEvent::Destroyed { operation });
                    match adapter.refresh() {
                        Ok(snapshot) => emit(WorkspaceDispatchEvent::Snapshot {
                            operation,
                            snapshot,
                        }),
                        Err(message) => {
                            emit(WorkspaceDispatchEvent::Failed { operation, message });
                        }
                    }
                }
                Err(message) => emit(WorkspaceDispatchEvent::Failed { operation, message }),
            },
        });
    }
}

/// The production adapter over the shared Repository-owned Pool module.
#[derive(Debug, Clone)]
pub struct RealWorkspaceDispatch {
    repository_root: PathBuf,
}

impl RealWorkspaceDispatch {
    /// Creates an adapter rooted at a discovered jj workspace.
    pub fn new(repository_root: impl Into<PathBuf>) -> Self {
        Self {
            repository_root: repository_root.into(),
        }
    }

    fn repository(&self) -> Result<wsg_core::Repository, String> {
        wsg_core::Repository::open(&self.repository_root).map_err(|error| error.to_string())
    }
}

impl WorkspaceDispatchAdapter for RealWorkspaceDispatch {
    fn refresh(&self) -> Result<WorkerPoolSnapshot, String> {
        Ok(self.repository()?.worker_pool().reconcile_runs())
    }

    fn resize(&self, capacity: usize) -> Result<PoolMutationResult, String> {
        let capacity = PoolCapacity::new(capacity).map_err(|error| error.to_string())?;
        let result = self
            .repository()?
            .worker_pool()
            .resize_to(capacity)
            .map_err(|error| error.to_string())?;
        Ok(PoolMutationResult {
            capacity: result.capacity().as_usize(),
            added_workers: result
                .added_workers()
                .iter()
                .map(ToString::to_string)
                .collect(),
            removed_workers: result
                .removed_workers()
                .iter()
                .map(ToString::to_string)
                .collect(),
        })
    }

    fn destroy(&self) -> Result<(), String> {
        self.repository()?
            .worker_pool()
            .destroy()
            .map_err(|error| error.to_string())
    }
}

/// Test adapter that records commands while returning a supplied snapshot.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct RecordingAdapter {
    commands: Arc<std::sync::Mutex<Vec<WorkspaceDispatchCommand>>>,
    snapshot: WorkerPoolSnapshot,
}

#[cfg(test)]
impl RecordingAdapter {
    pub(crate) fn new(snapshot: WorkerPoolSnapshot) -> Self {
        Self {
            commands: Arc::new(std::sync::Mutex::new(Vec::new())),
            snapshot,
        }
    }

    pub(crate) fn commands(&self) -> Vec<WorkspaceDispatchCommand> {
        self.commands
            .lock()
            .expect("recording adapter lock")
            .clone()
    }
}

#[cfg(test)]
impl WorkspaceDispatchAdapter for RecordingAdapter {
    fn refresh(&self) -> Result<WorkerPoolSnapshot, String> {
        self.commands
            .lock()
            .expect("recording adapter lock")
            .push(WorkspaceDispatchCommand::Refresh { operation: 0 });
        Ok(self.snapshot.clone())
    }

    fn resize(&self, capacity: usize) -> Result<PoolMutationResult, String> {
        self.commands.lock().expect("recording adapter lock").push(
            WorkspaceDispatchCommand::Resize {
                operation: 0,
                capacity,
            },
        );
        Ok(PoolMutationResult {
            capacity,
            added_workers: Vec::new(),
            removed_workers: Vec::new(),
        })
    }

    fn destroy(&self) -> Result<(), String> {
        self.commands
            .lock()
            .expect("recording adapter lock")
            .push(WorkspaceDispatchCommand::Destroy { operation: 0 });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn empty_snapshot() -> WorkerPoolSnapshot {
        let directory = TempDir::new().expect("temporary repository");
        let output = Command::new("jj")
            .args(["--config", "signing.behavior=drop", "git", "init"])
            .arg(directory.path())
            .output()
            .expect("jj should be installed");
        assert!(output.status.success());
        wsg_core::Repository::open(directory.path())
            .expect("repository should open")
            .read_worker_pool_snapshot()
    }

    #[test]
    fn controller_emits_a_snapshot_for_a_refresh_command() {
        let adapter = RecordingAdapter::new(empty_snapshot());
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let controller = WorkspaceDispatchController::new(adapter.clone(), move |event| {
            events_tx.send(event).expect("event receiver");
        });

        controller.submit(WorkspaceDispatchCommand::Refresh { operation: 7 });

        assert!(matches!(
            events_rx.recv().expect("refresh event"),
            WorkspaceDispatchEvent::Snapshot { operation: 7, .. }
        ));
        assert!(
            adapter.commands().iter().any(|command| matches!(
                command,
                WorkspaceDispatchCommand::Refresh { operation: 0 }
            ))
        );
    }
}
