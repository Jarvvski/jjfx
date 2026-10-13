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

use crate::app::{LiftJob, Msg, PendingWorkspace, STATUS_TTL, TidyKind};
use crate::config::ForgeConfig;
use crate::diff;
use crate::forge::{self, Target};
use crate::graph;
use crate::jj::{self, Jj};
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
    /// Focus the Workspace's terminal tab, opening one when it has none, and
    /// report [`Msg::WorkspaceOpened`].
    ///
    /// The decision itself needs a terminal query, so it runs beside the
    /// subprocess rather than inside the fold.
    OpenWorkspace {
        name: String,
        path: PathBuf,
        focus: bool,
        /// Whether the App opened this tab as part of creating the Workspace, so
        /// a failure reads as "created, but the tab failed" rather than a bare
        /// open error.
        created: bool,
    },
    /// Run a remote fetch and report [`Msg::Fetched`].
    Fetch,
    /// Re-project the Workspace list from disk and report
    /// [`Msg::StoreReloaded`].
    ///
    /// The projection shells out to `jj workspace list`, and the watcher fires
    /// this on every repository change, so it must not run in the fold.
    ReloadStore,
    /// Run one queued lift (a single workspace or all of them) and report
    /// [`Msg::Lifted`].
    Lift { job: LiftJob },
    /// Run one `tidy`/`tidyws` sweep (jj takes the repo lock) and report
    /// [`Msg::Tidied`].
    Tidy { kind: TidyKind },
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
    /// The blocking jj seam. Every mutation it performs holds the repo lock, so
    /// it is only ever called from a blocking worker, never from the fold.
    jj: Arc<dyn Jj>,
    forge: forge::Forge,
    /// Whether new workspaces are colocated with Git (jjfx's `colocate`).
    colocate: bool,
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
        colocate: bool,
        terminal: Arc<dyn Terminal>,
        jj: Arc<dyn Jj>,
        failure_log: Option<PathBuf>,
    ) -> Self {
        Self {
            tx,
            forge: forge::Forge::new(repo_root.clone(), forge),
            repo_root,
            colocate,
            terminal,
            jj,
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
            Effect::OpenWorkspace {
                name,
                path,
                focus,
                created,
            } => self.open_workspace(name, path, focus, created),
            Effect::Fetch => self.fetch(),
            Effect::ReloadStore => self.reload_store(),
            Effect::Lift { job } => self.lift(job),
            Effect::Tidy { kind } => self.tidy(kind),
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
        let colocate = self.colocate;
        tokio::spawn(async move {
            let progress_tx = tx.clone();
            let progress_workspace = workspace.clone();
            let requested_name = workspace.name().to_owned();
            let result = tokio::task::spawn_blocking(move || {
                Store::create_persisted_with_progress(
                    &repo_root,
                    &requested_name,
                    colocate,
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
        let colocate = self.colocate;
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
                let store = Store::load(&repo_root).with_colocate(colocate);
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

    /// Focus or open a Workspace's terminal tab on a blocking worker.
    ///
    /// `is_open`/`open`/`focus` all shell out to the multiplexer and `open` can
    /// sleep while it waits for a target, so none of it may run in the fold.
    fn open_workspace(&self, name: String, path: PathBuf, focus: bool, created: bool) {
        let tx = self.tx.clone();
        let terminal = Arc::clone(&self.terminal);
        tokio::spawn(async move {
            let opened = name.clone();
            let result = tokio::task::spawn_blocking(move || {
                if terminal.is_open(&opened) {
                    if focus {
                        terminal.focus(&opened)
                    } else {
                        Ok(())
                    }
                } else {
                    terminal.open(&opened, &path, focus)
                }
            })
            .await
            .unwrap_or_else(|e| Err(anyhow::anyhow!(e)));
            let _ = tx.send(Msg::WorkspaceOpened {
                name,
                created,
                result: result.map_err(|e| format!("{e:#}")),
            });
        });
    }

    /// Run one `tidy`/`tidyws` sweep on a blocking worker (jj takes the repo
    /// lock, so it must never run on the render loop), then report
    /// [`Msg::Tidied`].
    fn tidy(&self, kind: TidyKind) {
        let tx = self.tx.clone();
        let jj = Arc::clone(&self.jj);
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || match kind {
                TidyKind::Workspaces => jj.tidyws(),
                TidyKind::Abandon => jj.tidy(),
            })
            .await
            .unwrap_or_else(|e| Err(anyhow::anyhow!(e)));
            let _ = tx.send(Msg::Tidied {
                kind,
                result: result.map_err(|e| format!("{e:#}")),
            });
        });
    }

    /// Re-project the Workspace list on a blocking worker.
    ///
    /// `Store::load` opens the Repository and runs `jj workspace list`, so it may
    /// never run in the fold. The fresh projection replaces the App's store when
    /// it reports back, which is why the list is eventually - not instantly -
    /// consistent with disk.
    fn reload_store(&self) {
        let tx = self.tx.clone();
        let repo_root = self.repo_root.clone();
        let colocate = self.colocate;
        tokio::spawn(async move {
            let loaded = tokio::task::spawn_blocking(move || {
                Store::load(&repo_root).with_colocate(colocate)
            })
            .await;
            // `Store::load` degrades internally rather than failing, so a join
            // error means it panicked. Keep the current store and let the next
            // watcher event try again.
            if let Ok(store) = loaded {
                let _ = tx.send(Msg::StoreReloaded(store));
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeSet;
    use std::path::Path;

    use tokio::sync::mpsc::unbounded_channel;

    use crate::jj::RealJj;
    use crate::store::test_local_repo;

    /// A terminal double: deletion only closes a tab, which is a no-op here.
    #[derive(Default)]
    struct NoopTerminal;

    impl Terminal for NoopTerminal {
        fn is_open(&self, _name: &str) -> bool {
            false
        }

        fn open_workspaces(&self) -> anyhow::Result<BTreeSet<String>> {
            Ok(BTreeSet::new())
        }

        fn open(&self, _name: &str, _path: &Path, _focus: bool) -> anyhow::Result<()> {
            Ok(())
        }

        fn focus(&self, _name: &str) -> anyhow::Result<()> {
            Ok(())
        }

        fn close(&self, _name: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn deletion_reports_a_store_that_keeps_the_runtime_colocate_setting() {
        let repo_root = test_local_repo("runtime-colocate");
        let (tx, mut rx) = unbounded_channel();
        let runtime = Runtime::new(
            tx,
            repo_root.clone(),
            ForgeConfig::default(),
            false,
            Arc::new(NoopTerminal),
            Arc::new(RealJj::new(repo_root.clone())),
            None,
        );

        runtime.execute(Effect::DeleteWorkspace {
            workspace: Workspace {
                name: "absent".to_owned(),
                path: None,
            },
            operation: 1,
        });

        let message = rx.recv().await.expect("deletion should report back");
        let Msg::WorkspaceDeletionCompleted {
            store: Some(store), ..
        } = message
        else {
            panic!("expected a completed deletion with a store: {message:?}");
        };
        assert!(!store.colocate());

        let _ = std::fs::remove_dir_all(repo_root);
    }
}
