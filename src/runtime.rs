//! The effect runtime: performs the work [`App`] requests from its fold.
//!
//! [`App::handle`] folds a message and returns the effects it wants performed
//! outside the fold. Execution lives here - threads, blocking work, and the
//! channel back into the message loop - so the fold stays synchronous and tests
//! observe requests at the same seam the runtime executes them from.
//!
//! [`App::handle`]: crate::app::App::handle

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::app::{LiftJob, Msg, PendingWorkspace, STATUS_TTL};
use crate::config::ForgeConfig;
use crate::diff;
use crate::forge::{self, Target};
use crate::graph;
use crate::jj;
use crate::store::{Store, Workspace, WorkspaceCreateError};
use crate::terminal::Terminal;
use crate::workspace_dispatch::OperationId;

/// One piece of work the App wants performed outside the message fold.
#[derive(Debug)]
pub(crate) enum Effect {
    /// Clear the footer status once its generation is no longer current.
    ScheduleStatusExpiry { generation: u64 },
    /// Read one Workspace's trunk-to-`@` diff and report [`Msg::DiffLoaded`].
    LoadDiff { workspace: String },
    /// Read the commit graph and report [`Msg::GraphLoaded`].
    LoadGraph,
    /// Create one Workspace and report setup output and completion.
    CreateWorkspace { workspace: PendingWorkspace },
    /// Delete one Workspace and report a fresh projection.
    DeleteWorkspace {
        workspace: Workspace,
        operation: OperationId,
    },
    /// Run a remote fetch and report [`Msg::Fetched`].
    Fetch,
    /// Run one queued lift (a single workspace or all of them) and report
    /// [`Msg::Lifted`].
    Lift { job: LiftJob },
    /// Run the forge pipeline for these targets, streaming [`Msg::Forge`].
    StartForge { targets: Vec<Target> },
    /// Append a full failure (with context) to the on-disk setup-error log so a
    /// one-off, footer-truncated failure stays inspectable later.
    RecordFailure { context: String, message: String },
}

/// The one owner of effect execution for the running application.
pub(crate) struct Runtime {
    tx: UnboundedSender<Msg>,
    repo_root: PathBuf,
    terminal: Arc<dyn Terminal>,
    forge: forge::Forge,
    /// Where full failure text is appended; `None` disables logging (tests).
    failure_log: Option<PathBuf>,
}

impl Runtime {
    /// Bind the runtime to the message loop, repository, terminal, and config.
    /// `failure_log` is where [`Effect::RecordFailure`] appends full error text.
    pub(crate) fn new(
        tx: UnboundedSender<Msg>,
        repo_root: PathBuf,
        forge: ForgeConfig,
        terminal: Arc<dyn Terminal>,
        failure_log: Option<PathBuf>,
    ) -> Self {
        Self {
            tx,
            forge: forge::Forge::new(repo_root.clone(), forge),
            repo_root,
            terminal,
            failure_log,
        }
    }

    /// Perform one requested effect, reporting its outcome as a message.
    pub(crate) fn execute(&self, effect: Effect) {
        match effect {
            Effect::ScheduleStatusExpiry { generation } => self.schedule_status_expiry(generation),
            Effect::LoadDiff { workspace } => self.load_diff(workspace),
            Effect::LoadGraph => self.load_graph(),
            Effect::CreateWorkspace { workspace } => self.create_workspace(workspace),
            Effect::DeleteWorkspace {
                workspace,
                operation,
            } => self.delete_workspace(workspace, operation),
            Effect::Fetch => self.fetch(),
            Effect::Lift { job } => self.lift(job),
            Effect::StartForge { targets } => self.start_forge(targets),
            Effect::RecordFailure { context, message } => {
                if let Some(path) = &self.failure_log {
                    crate::events::append_failure(path, &context, &message);
                }
            }
        }
    }

    fn schedule_status_expiry(&self, generation: u64) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(STATUS_TTL).await;
            let _ = tx.send(Msg::StatusExpired(generation));
        });
    }

    fn load_diff(&self, workspace: String) {
        let tx = self.tx.clone();
        let repo_root = self.repo_root.clone();
        tokio::spawn(async move {
            let load_ws = workspace.clone();
            let files = tokio::task::spawn_blocking(move || diff::load(&repo_root, &load_ws))
                .await
                .unwrap_or_default();
            let _ = tx.send(Msg::DiffLoaded {
                ws: workspace,
                files,
            });
        });
    }

    fn load_graph(&self) {
        let tx = self.tx.clone();
        let repo_root = self.repo_root.clone();
        tokio::spawn(async move {
            if let Ok(Ok(graph)) =
                tokio::task::spawn_blocking(move || graph::load(&repo_root)).await
            {
                let _ = tx.send(Msg::GraphLoaded(graph));
            }
        });
    }

    fn create_workspace(&self, workspace: PendingWorkspace) {
        let tx = self.tx.clone();
        let repo_root = self.repo_root.clone();
        tokio::spawn(async move {
            let progress_tx = tx.clone();
            let progress_workspace = workspace.clone();
            let requested_name = workspace.name().to_owned();
            let result = tokio::task::spawn_blocking(move || {
                Store::create_persisted_with_progress(
                    &repo_root,
                    &requested_name,
                    move |stream, line| {
                        if !line.trim().is_empty() {
                            let _ = progress_tx.send(Msg::WorkspaceSetupOutput {
                                workspace: progress_workspace.clone(),
                                stream,
                                line: line.to_owned(),
                            });
                        }
                    },
                )
            })
            .await
            .map_err(|error| WorkspaceCreateError::create(format!("create failed: {error:#}")))
            .and_then(|result| result.map(|_| ()));
            let _ = tx.send(Msg::WorkspaceSetupCompleted { workspace, result });
        });
    }

    fn delete_workspace(&self, workspace: Workspace, operation: OperationId) {
        let tx = self.tx.clone();
        let repo_root = self.repo_root.clone();
        let known_path = workspace.path.clone();
        let terminal = Arc::clone(&self.terminal);
        let workspace_name = workspace.name.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                // Terminal presentation is best-effort, but it belongs in the
                // same blocking worker as jj and recursive filesystem cleanup.
                let _ = terminal.close(&workspace_name);
                let deletion =
                    Store::delete_persisted(&repo_root, &workspace_name, known_path.as_deref())
                        .map_err(|error| format!("{error:#}"));
                let store = Store::load(&repo_root);
                (deletion, store)
            })
            .await;
            let message = match result {
                Ok((result, store)) => Msg::WorkspaceDeletionCompleted {
                    operation,
                    workspace,
                    result,
                    store: Some(store),
                },
                Err(error) => Msg::WorkspaceDeletionCompleted {
                    operation,
                    workspace,
                    result: Err(format!("workspace deletion worker failed: {error:#}")),
                    store: None,
                },
            };
            let _ = tx.send(message);
        });
    }

    fn fetch(&self) {
        let tx = self.tx.clone();
        let repo_root = self.repo_root.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || jj::fetch(&repo_root))
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!(e)));
            let _ = tx.send(Msg::Fetched(result.map_err(|e| format!("{e:#}"))));
        });
    }

    /// Run one queued lift on a blocking worker (jj takes the repo lock, so it
    /// must never run on the render loop), then report it as [`Msg::Lifted`]. The
    /// App owns the serial queue; this executes exactly one job per effect.
    fn lift(&self, job: LiftJob) {
        let tx = self.tx.clone();
        let repo_root = self.repo_root.clone();
        tokio::spawn(async move {
            let worker_job = job.clone();
            let result = tokio::task::spawn_blocking(move || match worker_job {
                LiftJob::Workspace(ws) => jj::lift(&repo_root, &ws),
                LiftJob::All => jj::lift_all(&repo_root),
            })
            .await
            .unwrap_or_else(|e| Err(anyhow::anyhow!(e)));
            let _ = tx.send(Msg::Lifted {
                job,
                result: result.map_err(|e| format!("{e:#}")),
            });
        });
    }

    fn start_forge(&self, targets: Vec<Target>) {
        let mut updates = self.forge.start(targets);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            while let Some(update) = updates.recv().await {
                if tx.send(Msg::Forge(update)).is_err() {
                    break;
                }
            }
        });
    }
}
