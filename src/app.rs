//! Application state and the update/render logic. Background tasks send [`Msg`]
//! over a channel to the single owned `App`, which the main loop mutates and
//! redraws (the engine shape from the PRD).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ratatui::Frame;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, ListItem, Paragraph};
// sapling-renderdag exports its library as plain `renderdag`.
use renderdag::{Ancestor, GraphRowRenderer, Renderer};
use tokio::sync::mpsc::UnboundedSender;

use crate::agent::{self, AgentKind, AgentState};
use crate::attention::{self, Attention};
use crate::cmd::cmd;
use crate::config::{ForgeConfig, WorkspaceConfig};
use crate::diff::{self, FileDiff};
use crate::diff_view::Detail;
use crate::forge::{self, Target};
use crate::graph;
use crate::jj;
use crate::store::{self, Store, Workspace};
use crate::terminal::Terminal;
use crate::viewport::Viewport;
use crate::work::{Work, WorkState};
use crate::workspace_dispatch::{
    OperationId, RealWorkspaceDispatch, WorkerCommandResult, WorkerSessionOutcome,
    WorkspaceDispatchCommand, WorkspaceDispatchController, WorkspaceDispatchEvent,
};
use crate::workspace_list::{PresentationRow, WorkspaceList};
use wsg_core::{
    AgentSessionResolution, RunActivity, RunActivityKind, RunConclusion, RunResult, RunUsage,
    WorkerPoolSnapshot, WorkerSnapshot, WorkerStatus,
};

/// The focused Worker Pool management interaction.
enum PoolMode {
    View {
        selected: Option<String>,
    },
    Capacity {
        buffer: String,
        selected: Option<String>,
    },
    TicketInput {
        buffer: String,
        selected: Option<String>,
    },
    SendInput {
        worker: String,
        buffer: String,
    },
    AliasInput {
        worker: String,
        buffer: String,
    },
    DispatchPreview {
        tickets: Vec<String>,
        selected: Option<String>,
    },
    ReadyPreview {
        selected: Option<String>,
    },
    ParentInput {
        buffer: String,
    },
    LogDetail {
        worker: String,
    },
    ConfirmDispatchCapacity {
        tickets: Vec<String>,
        worker: Option<String>,
        shortage: crate::workspace_dispatch::DispatchCapacityShortage,
    },
}

/// What the key handler is currently collecting: normal navigation, a new
/// workspace name, or a delete confirmation.
enum Mode {
    Normal,
    NewWorkspace(String),
    ConfirmDelete(String),
    /// Confirming the destructive `tidy` sweep (abandon junk empties).
    ConfirmTidy,
    /// Focused Pool capacity and Worker management.
    Pool(PoolMode),
    /// Confirming a Pool shrink after exact capacity entry.
    ConfirmPoolShrink(usize, Option<String>),
    /// Confirming complete Pool destruction.
    ConfirmPoolDestroy,
    /// Confirming Reset for one Worker.
    ConfirmPoolReset(String),
    /// Confirming Dismiss for one Worker.
    ConfirmPoolDismiss(String),
    /// The `?` help overlay is open over the prior interaction mode.
    Help(Box<Mode>),
    /// The full-screen diff-detail view for one workspace (ADR 0008).
    Detail(Detail),
    /// The full-screen "world" commit graph: the repo DAG laid out like
    /// `jj log` (ticket 11). The rendered lines are rebuilt each draw from
    /// `App::graph`, so only the [`Viewport`] offset is held here.
    Graph(Viewport),
}

/// Normal-mode keybindings shown by the `?` help overlay. Keep this adjacent
/// to the corresponding dispatch so the normal interaction contract is local.
const NORMAL_BINDINGS: &[(&str, &str)] = &[
    ("Move down", "j / ↓"),
    ("Move up", "k / ↑"),
    ("Open workspace", "enter"),
    ("Open in background", "o"),
    ("Diff detail", "→ / l"),
    ("Toggle world graph pane", "w"),
    ("Scroll world pane", "J / K"),
    ("World graph (full screen)", "W"),
    ("New workspace", "n"),
    ("Delete workspace", "d"),
    ("Forge selected", "f"),
    ("Forge all", "F"),
    ("Forge default", "g"),
    ("Fetch from remote", "u"),
    ("Lift onto trunk", "r"),
    ("Lift all onto trunk", "R"),
    ("Tidy this workspace", "t"),
    ("Tidy (abandon junk empties)", "T"),
    ("Worker Pool management", "p"),
    ("Fold / expand idle group", "c"),
    ("Toggle this help", "?"),
    ("Quit", "q / esc"),
];

/// Worker Pool keybindings shown by the `?` help overlay. Pool actions are
/// deliberately separate from normal mode because several keys have different
/// meanings there (notably `d` and `o`).
const POOL_BINDINGS: &[(&str, &str)] = &[
    ("Move between Workers", "j / k / ↑ / ↓"),
    ("Ticket Dispatch", "d"),
    ("Send to Worker", "s"),
    ("Review Worker", "v"),
    ("Rebase Worker", "g"),
    ("Open Pull Request", "P"),
    ("Edit Worker alias", "e"),
    ("Dismiss Worker", "x"),
    ("Worker log", "l"),
    ("Ready Ticket discovery", "a"),
    ("Dispatch Group orchestration", "o"),
    ("Resize Pool", "r"),
    ("Reset Worker", "K"),
    ("Destroy Pool", "D"),
    ("Toggle this help", "?"),
    ("Back", "q / esc"),
];

/// The identity of the single workspace whose on-create command is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingWorkspace {
    name: String,
    path: std::path::PathBuf,
}

/// Messages folded into the app from the terminal and background watchers.
#[derive(Debug)]
pub enum Msg {
    /// A terminal input event (key, resize, ...).
    Input(Event),
    /// The repo changed on disk; re-reconcile the store.
    Reload,
    /// A Claude Code hook event, appended to the global log (ADR 0004).
    AgentEvent(agent::Event),
    /// A freshly computed work-lifecycle snapshot, keyed by workspace name.
    WorkSnapshot(HashMap<String, Work>),
    /// A Workspace Dispatch controller event.
    WorkspaceDispatch(WorkspaceDispatchEvent),
    /// A forge pipeline transition (ticket 08).
    Forge(forge::Update),
    /// The background `jj git fetch` finished; `Err` carries jj's error text.
    Fetched(Result<(), String>),
    /// The configured command for a newly-created workspace finished.
    WorkspaceConfigured {
        workspace: PendingWorkspace,
        result: Result<(), String>,
    },
    /// The diff for a workspace finished loading (ticket 10).
    DiffLoaded { ws: String, files: Vec<FileDiff> },
    /// The commit graph finished loading from jj-lib (ticket 11).
    GraphLoaded(graph::Graph),
    /// A footer status message's expiry timer fired. Carries the generation it
    /// was armed for; a stale one (an action replaced the message since) is a
    /// no-op.
    StatusExpired(u64),
    /// The animation ticker fired; advance the working-glyph frame
    /// ([`App::animate`]).
    Tick,
}

/// How long a transient footer status message stays before expiring.
const STATUS_TTL: Duration = Duration::from_secs(5);

/// Run the configured new-workspace command directly from `path`. An empty
/// argv disables the hook; otherwise its first item is the program and the rest
/// are passed unchanged as arguments, with no shell interpretation.
pub(crate) fn run_on_create(command: &[String], path: &std::path::Path) -> anyhow::Result<()> {
    let Some((program, args)) = command.split_first() else {
        return Ok(());
    };
    cmd(program).args(args).current_dir(path).run()?.checked()?;
    Ok(())
}

/// Startup settings owned by the App rather than its terminal or store.
#[derive(Debug, Default)]
pub struct AppConfig {
    /// How newly-created workspaces are prepared before their tabs open.
    pub workspace: WorkspaceConfig,
    /// How the forge pipeline opens and maintains pull requests.
    pub forge: ForgeConfig,
}

/// The whole application state - one owned value on the main task.
pub struct App {
    store: Store,
    /// Current agent lifecycle state per workspace. Owns the fold and the canon
    /// join (the `cwd` <-> workspace-path key), so startup and live updates
    /// cannot drift. Absent workspaces are simply missing.
    agents: agent::AgentStates,
    /// Latest work-lifecycle snapshot per workspace, keyed by workspace name.
    /// Missing entries render as unknown until the first snapshot arrives.
    work: HashMap<String, Work>,
    /// Latest immutable Worker Pool presentation snapshot.
    worker_pool: Option<WorkerPoolSnapshot>,
    /// The deep Workspace Dispatch seam for Pool commands and events.
    dispatch: WorkspaceDispatchController,
    /// Operation generation used to reject stale mutation results.
    next_operation: OperationId,
    active_operation: Option<OperationId>,
    /// A Reset keeps its operation active until Workspace restoration completes.
    worker_reset_operation: Option<OperationId>,
    /// Independent operation identity for durable Parent orchestration.
    orchestration_operation: Option<OperationId>,
    /// Independent operation identity for the selected Worker log watcher.
    worker_log_operation: Option<OperationId>,
    /// Latest immutable provider-neutral Worker log snapshot.
    worker_log: Option<crate::workspace_dispatch::WorkerLogSnapshot>,
    /// The latest selected-Worker Follow-up Session outcome.
    worker_session: Option<WorkerSessionOutcome>,
    /// The latest typed non-Run Worker action result.
    worker_command_result: Option<WorkerCommandResult>,
    /// Latest Worker log failure retained for the focused detail view.
    worker_log_error: Option<String>,
    /// Latest ordered Direct Dispatch outcomes for the Pool view.
    dispatch_result: Option<crate::workspace_dispatch::DispatchResult>,
    /// Latest Ready Ticket discovery result awaiting preview or launch.
    ready_tickets: Option<crate::workspace_dispatch::ReadyTicketResult>,
    /// Latest immutable Dispatch Group presentation projection.
    group_progress: Option<crate::workspace_dispatch::DispatchGroupProgress>,
    /// Live forge progress per workspace, keyed by name. An entry exists only
    /// while a forge runs.
    forge_progress: HashMap<String, forge::Progress>,
    /// A background `jj git fetch` is in flight; a second `u` is ignored until
    /// it resolves (two would just contend on the repo lock).
    fetching: bool,
    /// User-owned command run once after workspace creation. Empty disables it.
    workspace_config: WorkspaceConfig,
    /// The single new workspace whose configured command is still running.
    pending_workspace: Option<PendingWorkspace>,
    /// The deep Forge module owns execution, progress, and Pull Request rules.
    forge: forge::Forge,
    /// Channel to the app's own message loop, so forge tasks can stream updates
    /// back as [`Msg::Forge`].
    tx: UnboundedSender<Msg>,
    /// The multiplexer jjfx drives for workspace tabs (behind a trait so kitty is
    /// swappable - ticket 07).
    terminal: Box<dyn Terminal>,
    /// The jj mutations the destructive verbs perform, behind a trait so they are
    /// testable against a fake (like `terminal`), rather than shelling out inline.
    jj: Box<dyn jj::Jj>,
    mode: Mode,
    /// The last-loaded commit graph (ticket 11), shared by the world view and the
    /// per-workspace strip in the detail view. `None` until first loaded.
    graph: Option<graph::Graph>,
    /// The inline world-graph pane under the home list: `Some` (holding its
    /// scroll viewport) when toggled on. The toggle persists across launches as
    /// [`crate::ui_state::UiState::world_pane`].
    world: Option<Viewport>,
    /// A transient one-line message shown in the footer (last action's result).
    status: Option<String>,
    /// Bumped whenever the status is replaced, so an expiry timer armed for an
    /// older message cannot clear a newer one.
    status_gen: u64,
    /// The attention-grouped, idle-collapsible workspace list: owns grouping,
    /// the idle fold, and the name-tracked selection + render cursor. `App`
    /// supplies the [`Attention`] per workspace via [`App::classified`].
    list: WorkspaceList,
    /// The working-glyph animation frame counter, advanced by [`App::animate`]
    /// on each [`Msg::Tick`].
    tick: u64,
    pub should_quit: bool,
}

impl App {
    pub fn new(
        store: Store,
        agents: agent::AgentStates,
        terminal: Box<dyn Terminal>,
        jj: Box<dyn jj::Jj>,
        config: AppConfig,
        world_pane: bool,
        tx: UnboundedSender<Msg>,
    ) -> Self {
        let forge = forge::Forge::new(store.repo_root().to_path_buf(), config.forge);
        let dispatch_tx = tx.clone();
        let dispatch = WorkspaceDispatchController::new(
            RealWorkspaceDispatch::new(store.repo_root()),
            move |event| {
                let _ = dispatch_tx.send(Msg::WorkspaceDispatch(event));
            },
        );
        let mut app = App {
            store,
            agents,
            work: HashMap::new(),
            worker_pool: None,
            dispatch,
            next_operation: 0,
            active_operation: None,
            worker_reset_operation: None,
            orchestration_operation: None,
            worker_log_operation: None,
            worker_log: None,
            worker_session: None,
            worker_command_result: None,
            worker_log_error: None,
            dispatch_result: None,
            ready_tickets: None,
            group_progress: None,
            forge_progress: HashMap::new(),
            fetching: false,
            workspace_config: config.workspace,
            pending_workspace: None,
            forge,
            tx,
            terminal,
            jj,
            mode: Mode::Normal,
            graph: None,
            world: world_pane.then(Viewport::default),
            status: None,
            status_gen: 0,
            list: WorkspaceList::default(),
            tick: 0,
            should_quit: false,
        };
        app.ensure_selection();
        app
    }

    /// Fold one message into the state.
    pub fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Input(Event::Key(key)) => self.on_key(key),
            Msg::Input(_) => {}
            Msg::Reload => self.reload(),
            Msg::AgentEvent(ev) => self.on_agent_event(ev),
            Msg::WorkSnapshot(work) => self.work = work,
            Msg::WorkspaceDispatch(event) => self.on_workspace_dispatch(event),
            Msg::Forge(update) => self.on_forge(update),
            Msg::Fetched(result) => self.on_fetched(result),
            Msg::WorkspaceConfigured { workspace, result } => {
                self.on_workspace_configured(workspace, result);
            }
            Msg::DiffLoaded { ws, files } => self.on_diff_loaded(ws, files),
            Msg::GraphLoaded(graph) => self.graph = Some(graph),
            Msg::StatusExpired(generation) => {
                if generation == self.status_gen {
                    self.status = None;
                }
            }
            Msg::Tick => {
                self.animate();
            }
        }
    }

    fn on_worker_pool_snapshot(&mut self, operation: OperationId, snapshot: WorkerPoolSnapshot) {
        if operation == 0 {
            if self.active_operation.is_some() {
                return;
            }
        } else if self.active_operation != Some(operation) {
            return;
        }
        if self.worker_pool.as_ref() == Some(&snapshot) {
            if operation != 0 {
                self.active_operation = None;
            }
            return;
        }
        self.worker_pool = Some(snapshot);
        if operation != 0 && self.worker_reset_operation != Some(operation) {
            self.active_operation = None;
        }
    }

    fn on_workspace_dispatch(&mut self, event: WorkspaceDispatchEvent) {
        match event {
            WorkspaceDispatchEvent::Snapshot {
                operation,
                snapshot,
            } => self.on_worker_pool_snapshot(operation, snapshot),
            WorkspaceDispatchEvent::Resized { operation, result } => {
                if self.active_operation == Some(operation) {
                    self.set_status(format!(
                        "Pool resized to {} ({} added, {} removed)",
                        result.capacity(),
                        result.added_workers().len(),
                        result.removed_workers().len()
                    ));
                }
            }
            WorkspaceDispatchEvent::Destroyed { operation } => {
                if self.active_operation == Some(operation) {
                    self.set_status("Worker Pool destroyed".to_string());
                    self.mode = Mode::Normal;
                }
            }
            WorkspaceDispatchEvent::Dispatched { operation, result } => {
                if self.active_operation == Some(operation) {
                    let count = result.outcomes().len();
                    let partial = result.is_partial();
                    self.dispatch_result = Some(result);
                    self.active_operation = None;
                    self.refresh_worker_pool();
                    self.set_status(if partial {
                        format!("Dispatched {count} Ticket(s) with partial capacity")
                    } else {
                        format!("Dispatched {count} Ticket(s)")
                    });
                }
            }
            WorkspaceDispatchEvent::DispatchCapacity {
                operation,
                tickets,
                worker,
                shortage,
            } => {
                if self.active_operation == Some(operation) {
                    self.mode = Mode::Pool(PoolMode::ConfirmDispatchCapacity {
                        tickets,
                        worker,
                        shortage,
                    });
                    self.set_status(format!(
                        "Dispatch needs {} idle Worker(s); only {} available",
                        shortage.requested(),
                        shortage.available()
                    ));
                }
            }
            WorkspaceDispatchEvent::ReadyTickets { operation, result } => {
                if self.active_operation == Some(operation) {
                    let count = result.tickets().len();
                    self.ready_tickets = Some(result);
                    self.active_operation = None;
                    self.mode = Mode::Pool(PoolMode::ReadyPreview { selected: None });
                    self.set_status(format!("Found {count} Ready Ticket(s)"));
                }
            }
            WorkspaceDispatchEvent::GroupProgress {
                operation,
                progress,
            } => {
                if operation == 0
                    || self.active_operation == Some(operation)
                    || self.orchestration_operation == Some(operation)
                {
                    self.group_progress = Some(progress);
                }
            }
            WorkspaceDispatchEvent::OrchestrationStarted {
                operation,
                parent,
                resumed,
            } => {
                if self.orchestration_operation == Some(operation) {
                    self.set_status(format!(
                        "{} Dispatch Group {parent}",
                        if resumed { "Resuming" } else { "Starting" }
                    ));
                }
            }
            WorkspaceDispatchEvent::OrchestrationProgress {
                operation,
                progress,
                notice,
            } => {
                if self.orchestration_operation == Some(operation) {
                    self.group_progress = Some(progress);
                    if let Some(notice) = notice {
                        self.set_status(notice);
                    }
                }
            }
            WorkspaceDispatchEvent::OrchestrationDirect {
                operation,
                parent,
                worker,
                pid,
            } => {
                if self.orchestration_operation == Some(operation) {
                    self.orchestration_operation = None;
                    self.set_status(format!(
                        "Dispatched Parent {parent} to {worker} (PID {pid})"
                    ));
                }
            }
            WorkspaceDispatchEvent::OrchestrationTerminal {
                operation,
                parent,
                counts,
            } => {
                if self.orchestration_operation == Some(operation) {
                    self.orchestration_operation = None;
                    self.set_status(format!(
                        "Dispatch Group {parent} complete: {} done, {} failed, {} skipped",
                        counts.done(),
                        counts.failed(),
                        counts.skipped()
                    ));
                }
            }
            WorkspaceDispatchEvent::WorkerActionCompleted { operation, outcome } => {
                if self.active_operation == Some(operation) {
                    let worker = outcome.worker().to_owned();
                    let action = outcome.action();
                    self.worker_session = Some(outcome);
                    self.active_operation = None;
                    self.refresh_worker_pool();
                    self.set_status(format!(
                        "{} {} (PID {})",
                        action.as_str(),
                        worker,
                        self.worker_session
                            .as_ref()
                            .map_or(0, WorkerSessionOutcome::pid)
                    ));
                }
            }
            WorkspaceDispatchEvent::WorkerResetCompleted { operation, outcome } => {
                if self.active_operation == Some(operation) {
                    self.set_status(format!(
                        "Reset {} ({:?}) to idle",
                        outcome.worker(),
                        outcome.run()
                    ));
                    self.refresh_worker_pool();
                }
            }
            WorkspaceDispatchEvent::WorkerCommandCompleted { operation, result } => {
                if self.active_operation == Some(operation) {
                    self.worker_command_result = Some(result.clone());
                    self.active_operation = None;
                    self.refresh_worker_pool();
                    self.set_status(result.notice());
                }
            }
            WorkspaceDispatchEvent::WorkspaceRestorationCompleted {
                operation,
                worker,
                result,
            } => {
                if self.worker_reset_operation == Some(operation) {
                    self.worker_reset_operation = None;
                    self.active_operation = None;
                    self.set_status(match result {
                        crate::workspace_dispatch::WorkspaceRestorationResult::Skipped => {
                            format!("Reset {worker}: Workspace restoration skipped")
                        }
                        crate::workspace_dispatch::WorkspaceRestorationResult::Restored => {
                            format!("Reset {worker}: Workspace restored")
                        }
                        crate::workspace_dispatch::WorkspaceRestorationResult::Failed(error) => {
                            format!("Reset {worker}: Workspace restoration failed: {error}")
                        }
                    });
                    self.refresh_worker_pool();
                }
            }
            WorkspaceDispatchEvent::WorkerLogUpdated {
                operation,
                snapshot,
            } => {
                if self.worker_log_operation == Some(operation) {
                    let terminal = snapshot.result().is_some();
                    self.worker_log = Some(*snapshot);
                    self.worker_log_error = None;
                    if terminal {
                        self.worker_log_operation = None;
                    }
                }
            }
            WorkspaceDispatchEvent::WorkerLogFailed {
                operation,
                worker: _,
                message,
            } => {
                if self.worker_log_operation == Some(operation) {
                    self.worker_log_operation = None;
                    self.worker_log_error = Some(message);
                }
            }
            WorkspaceDispatchEvent::Failed { operation, message } => {
                if self.active_operation == Some(operation) {
                    self.active_operation = None;
                    self.worker_reset_operation = None;
                    self.set_status(format!("Worker Pool: {message}"));
                } else if self.orchestration_operation == Some(operation) {
                    self.orchestration_operation = None;
                    self.set_status(format!("Dispatch Group: {message}"));
                }
            }
        }
    }

    pub(crate) fn workspace_dispatch_controller(&self) -> WorkspaceDispatchController {
        self.dispatch.clone()
    }

    pub(crate) fn refresh_worker_pool(&self) {
        self.dispatch
            .submit(WorkspaceDispatchCommand::Refresh { operation: 0 });
    }

    fn begin_dispatch(&mut self) -> OperationId {
        self.next_operation = self.next_operation.wrapping_add(1).max(1);
        self.active_operation = Some(self.next_operation);
        self.next_operation
    }

    fn orchestrate_parent(&mut self, parent: String) {
        self.next_operation = self.next_operation.wrapping_add(1).max(1);
        let operation = self.next_operation;
        self.orchestration_operation = Some(operation);
        self.pin_status(format!("starting Dispatch Group {parent}..."));
        self.dispatch
            .submit(WorkspaceDispatchCommand::Orchestrate { operation, parent });
    }

    fn start_worker_log(&mut self, worker: String) {
        self.next_operation = self.next_operation.wrapping_add(1).max(1);
        let operation = self.next_operation;
        self.worker_log_operation = Some(operation);
        self.worker_log = None;
        self.worker_log_error = None;
        self.mode = Mode::Pool(PoolMode::LogDetail {
            worker: worker.clone(),
        });
        self.pin_status(format!("reading Worker {worker} log..."));
        self.dispatch
            .submit(WorkspaceDispatchCommand::WatchWorkerLog { operation, worker });
    }

    fn stop_worker_log(&mut self) {
        if let Some(operation) = self.worker_log_operation.take() {
            self.dispatch
                .submit(WorkspaceDispatchCommand::StopWorkerLog { operation });
        }
    }

    /// Advance the working-glyph animation one frame. Returns whether anything
    /// on screen is actually animating - the workspace list is visible and some
    /// agent is mid-turn - so the event loop can skip the redraw for every
    /// other tick instead of repainting an idle screen ~7 times a second.
    pub fn animate(&mut self) -> bool {
        self.tick = self.tick.wrapping_add(1);
        matches!(self.mode, Mode::Normal | Mode::Help(_))
            && (self
                .store
                .workspaces()
                .iter()
                .any(|w| self.agent_state(w) == AgentState::Working)
                || self.worker_pool.as_ref().is_some_and(|snapshot| {
                    snapshot
                        .workers()
                        .iter()
                        .any(|worker| worker.status() == WorkerStatus::Busy)
                }))
    }

    /// Fold a freshly-loaded diff into the detail view, if it is still open for
    /// the same workspace (the user may have backed out or switched meanwhile).
    fn on_diff_loaded(&mut self, ws: String, files: Vec<FileDiff>) {
        if let Mode::Detail(d) = &mut self.mode
            && d.workspace() == ws
        {
            d.loaded(files);
        }
    }

    /// Show a transient footer message and arm a timer to clear it after
    /// [`STATUS_TTL`]. The generation stamp disarms any earlier timer, so an old
    /// expiry can never wipe a newer message.
    fn set_status(&mut self, msg: String) {
        self.pin_status(msg);
        let generation = self.status_gen;
        let tx = self.tx.clone();
        // Unit tests drive `handle` without a tokio runtime; there the message
        // simply never expires, which keeps assertions on `status` simple.
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                tokio::time::sleep(STATUS_TTL).await;
                let _ = tx.send(Msg::StatusExpired(generation));
            });
        }
    }

    /// Show a footer message that stays until replaced - for in-flight progress
    /// ("fetching…") whose end is signalled by a later message, not a timer.
    fn pin_status(&mut self, msg: String) {
        self.status_gen += 1;
        self.status = Some(msg);
    }

    /// Project one already-folded Forge snapshot into presentation state.
    fn on_forge(&mut self, update: forge::Update) {
        match update {
            forge::Update::Progress {
                workspace,
                progress,
                notice,
            } => {
                debug_assert!(
                    notice
                        .as_deref()
                        .map(|notice| progress.reason() == Some(notice))
                        .unwrap_or(true),
                    "a Forge notice must match the retained progress reason"
                );
                self.forge_progress.insert(workspace.clone(), progress);
                if let Some(notice) = notice {
                    self.set_status(format!("{workspace}: {notice}"));
                }
            }
            forge::Update::Finished {
                workspace,
                progress,
            } => {
                self.forge_progress.remove(&workspace);
                if progress.is_none() {
                    self.set_status(format!("{workspace}: forged"));
                }
                // A forge moves revisions (weld/push); refresh the graph if shown.
                self.refresh_graph_if_visible();
            }
            forge::Update::Aborted { reason } => {
                // Drop every still-running overlay; the run did no per-ws work.
                self.forge_progress.retain(|_, progress| !progress.active());
                self.set_status(format!("forge aborted: {reason}"));
            }
        }
    }

    /// Fold one hook event into the per-workspace agent state.
    fn on_agent_event(&mut self, ev: agent::Event) {
        self.agents.apply(&ev);
    }

    /// The agent state for a workspace, `Absent` if the log has none for it.
    fn agent_state(&self, w: &Workspace) -> AgentState {
        self.agent_of(w).state
    }

    /// The live agent (state + kind) for a workspace, default if the log has
    /// no events for it.
    fn agent_of(&self, w: &Workspace) -> agent::Agent {
        w.path
            .as_deref()
            .map(|p| self.agents.agent_for(p))
            .unwrap_or_default()
    }

    /// The work state for a workspace, `Unknown` until the first snapshot lands.
    fn work_state(&self, w: &Workspace) -> WorkState {
        self.work
            .get(&w.name)
            .map(|wk| wk.state)
            .unwrap_or_default()
    }

    /// How far the workspace is behind `trunk()`, `0` until the first snapshot.
    fn behind(&self, w: &Workspace) -> u32 {
        self.work.get(&w.name).map(|wk| wk.behind).unwrap_or(0)
    }

    fn on_key(&mut self, key: KeyEvent) {
        // Only react to presses; crossterm can also deliver Release/Repeat.
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.code == KeyCode::Char('?') && !matches!(self.mode, Mode::Help(_)) {
            let previous = std::mem::replace(&mut self.mode, Mode::Normal);
            self.mode = Mode::Help(Box::new(previous));
            return;
        }
        match &self.mode {
            Mode::Normal => self.on_key_normal(key),
            Mode::NewWorkspace(_) => self.on_key_new_workspace(key),
            Mode::ConfirmDelete(_) => self.on_key_confirm_delete(key),
            Mode::ConfirmTidy => self.on_key_confirm_tidy(key),
            Mode::Pool(_) => self.on_key_pool(key),
            Mode::ConfirmPoolShrink(_, _) => self.on_key_confirm_pool_shrink(key),
            Mode::ConfirmPoolDestroy => self.on_key_confirm_pool_destroy(key),
            Mode::ConfirmPoolReset(_) => self.on_key_confirm_pool_reset(key),
            Mode::ConfirmPoolDismiss(_) => self.on_key_confirm_pool_dismiss(key),
            Mode::Help(_) => self.on_key_help(key),
            Mode::Detail(_) => self.on_key_detail(key),
            Mode::Graph(_) => self.on_key_graph(key),
        }
    }

    fn on_key_normal(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('n') => {
                if self.refuse_while_configuring() {
                    return;
                }
                self.status = None;
                self.mode = Mode::NewWorkspace(String::new());
            }
            KeyCode::Enter => self.open_selected(true),
            KeyCode::Char('o') => self.open_selected(false),
            KeyCode::Right | KeyCode::Char('l') => self.open_detail(),
            KeyCode::Char('w') => self.toggle_world(),
            KeyCode::Char('W') => self.open_graph(),
            KeyCode::Char('J') => {
                if let Some(v) = &mut self.world {
                    v.line_down();
                }
            }
            KeyCode::Char('K') => {
                if let Some(v) = &mut self.world {
                    v.line_up();
                }
            }
            KeyCode::Char('d') => self.begin_delete_selected(),
            KeyCode::Char('r') => self.lift_selected(),
            KeyCode::Char('R') => self.lift_all(),
            KeyCode::Char('t') => self.tidyws(),
            KeyCode::Char('T') => self.begin_tidy(),
            KeyCode::Char('u') => self.fetch(),
            KeyCode::Char('f') => self.forge_selected(),
            KeyCode::Char('F') => self.forge_all(),
            KeyCode::Char('g') => self.forge_default(),
            KeyCode::Char('p') => self.open_pool(),
            KeyCode::Char('c') => {
                self.list.toggle_idle();
                self.ensure_selection();
            }
            _ => {}
        }
    }

    /// Help is a read-only overlay: `?` or `esc` dismisses it, everything else
    /// is swallowed so no navigation leaks through.
    fn on_key_help(&mut self, key: KeyEvent) {
        if matches!(key.code, KeyCode::Char('?') | KeyCode::Esc) {
            let Mode::Help(previous) = std::mem::replace(&mut self.mode, Mode::Normal) else {
                return;
            };
            self.mode = *previous;
        }
    }

    /// Forward a key to the open diff viewer; close it when the viewer signals
    /// exit. The viewer owns all diff-detail key behaviour ([`Detail::on_key`]).
    fn on_key_detail(&mut self, key: KeyEvent) {
        if let Mode::Detail(d) = &mut self.mode
            && d.on_key(key)
        {
            self.mode = Mode::Normal;
        }
    }

    /// `→`/`l`: open the diff-detail view for the selected workspace and kick off
    /// an async read of its diff from trunk (a blocking jj read on a worker
    /// thread, so a large patch never stalls the render loop).
    fn open_detail(&mut self) {
        self.status = None;
        let Some(w) = self.selected_workspace().cloned() else {
            return;
        };
        self.mode = Mode::Detail(Detail::loading(w.name.clone()));
        let tx = self.tx.clone();
        let repo_root = self.store.repo_root().to_path_buf();
        let ws = w.name;
        tokio::spawn(async move {
            let load_ws = ws.clone();
            let files = tokio::task::spawn_blocking(move || diff::load(&repo_root, &load_ws))
                .await
                .unwrap_or_default();
            let _ = tx.send(Msg::DiffLoaded { ws, files });
        });
        // The detail view carries a per-workspace graph strip; load the graph
        // alongside the diff so the strip fills in.
        self.spawn_graph_load();
    }

    /// `w`: toggle the inline world-graph pane under the home list. Turning it
    /// on kicks off an async jj-lib read; the last-loaded graph (if any) shows
    /// immediately while the fresh one loads.
    fn toggle_world(&mut self) {
        match self.world {
            Some(_) => self.world = None,
            None => {
                self.world = Some(Viewport::default());
                self.spawn_graph_load();
            }
        }
    }

    /// Whether the inline world-graph pane is on - persisted as UI state at exit.
    pub fn world_pane(&self) -> bool {
        self.world.is_some()
    }

    /// `W`: open the full-screen world graph and kick off an async jj-lib read.
    /// The last-loaded graph shows immediately (if any) while the fresh one loads.
    fn open_graph(&mut self) {
        self.status = None;
        self.mode = Mode::Graph(Viewport::default());
        self.spawn_graph_load();
    }

    /// World-graph keys: `j`/`k` (and arrows/page) scroll; `esc`/`W`/`q` return to
    /// the list. The highlighted chain is whatever workspace was selected.
    fn on_key_graph(&mut self, key: KeyEvent) {
        let Mode::Graph(g) = &mut self.mode else {
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('W') => self.mode = Mode::Normal,
            KeyCode::Char('j') | KeyCode::Down => g.line_down(),
            KeyCode::Char('k') | KeyCode::Up => g.line_up(),
            KeyCode::PageDown => g.page_down(),
            KeyCode::PageUp => g.page_up(),
            KeyCode::Char('g') => g.jump_top(),
            KeyCode::Char('G') => g.jump_bottom(),
            _ => {}
        }
    }

    /// Spawn a blocking jj-lib graph read on a worker thread (never the render
    /// loop) and stream the result back as [`Msg::GraphLoaded`]. On error the last
    /// graph simply stays; a graph read must never crash the TUI.
    fn spawn_graph_load(&self) {
        let tx = self.tx.clone();
        let repo_root = self.store.repo_root().to_path_buf();
        tokio::spawn(async move {
            if let Ok(Ok(graph)) =
                tokio::task::spawn_blocking(move || graph::load(&repo_root)).await
            {
                let _ = tx.send(Msg::GraphLoaded(graph));
            }
        });
    }

    /// Reload the graph if a graph-bearing view is on screen (the full-screen
    /// views or the inline world pane), so it tracks the underlying revisions
    /// changing (new commits, fetch, forge). Also called once at startup so a
    /// persisted-on world pane fills in. `pub(crate)` for that startup call.
    pub(crate) fn refresh_graph_if_visible(&self) {
        if matches!(self.mode, Mode::Graph(_) | Mode::Detail(_)) || self.world.is_some() {
            self.spawn_graph_load();
        }
    }

    fn open_pool(&mut self) {
        self.mode = Mode::Pool(PoolMode::View { selected: None });
        self.refresh_worker_pool();
    }

    fn on_key_pool(&mut self, key: KeyEvent) {
        let mode = std::mem::replace(&mut self.mode, Mode::Normal);
        match mode {
            Mode::Pool(PoolMode::View { selected }) => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {}
                KeyCode::Char('j') | KeyCode::Down => {
                    let selected = self.pool_worker_after(selected.as_deref(), 1);
                    self.mode = Mode::Pool(PoolMode::View { selected });
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    let selected = self.pool_worker_after(selected.as_deref(), -1);
                    self.mode = Mode::Pool(PoolMode::View { selected });
                }
                KeyCode::Char('d') => {
                    let selected = selected.or_else(|| self.pool_worker_after(None, 0));
                    self.mode = Mode::Pool(PoolMode::TicketInput {
                        buffer: String::new(),
                        selected,
                    });
                }
                KeyCode::Char('s') => {
                    let Some(worker) = selected.clone().or_else(|| self.pool_worker_after(None, 0))
                    else {
                        self.set_status("Select a Worker before Send".to_owned());
                        self.mode = Mode::Pool(PoolMode::View { selected });
                        return;
                    };
                    if !self.worker_is_available_for_action(&worker) {
                        self.set_status("Worker is busy; Send is unavailable".to_owned());
                        self.mode = Mode::Pool(PoolMode::View {
                            selected: Some(worker),
                        });
                    } else {
                        self.mode = Mode::Pool(PoolMode::SendInput {
                            worker,
                            buffer: String::new(),
                        });
                    }
                }
                KeyCode::Char('v') => {
                    let Some(worker) = selected.clone().or_else(|| self.pool_worker_after(None, 0))
                    else {
                        self.set_status("Select a Worker before Review".to_owned());
                        self.mode = Mode::Pool(PoolMode::View { selected });
                        return;
                    };
                    if !self.worker_is_available_for_action(&worker) {
                        self.set_status("Worker is busy; Review is unavailable".to_owned());
                        self.mode = Mode::Pool(PoolMode::View {
                            selected: Some(worker),
                        });
                    } else {
                        self.mode = Mode::Pool(PoolMode::View {
                            selected: Some(worker.clone()),
                        });
                        self.review_worker(worker);
                    }
                }
                KeyCode::Char('e') => {
                    let Some(worker) = selected.clone().or_else(|| self.pool_worker_after(None, 0))
                    else {
                        self.set_status("Select a Worker before editing its alias".to_owned());
                        self.mode = Mode::Pool(PoolMode::View { selected });
                        return;
                    };
                    let alias = self
                        .worker_pool
                        .as_ref()
                        .and_then(|snapshot| snapshot.worker(&worker))
                        .map_or_else(String::new, |worker| worker.alias().to_owned());
                    self.mode = Mode::Pool(PoolMode::AliasInput {
                        worker,
                        buffer: alias,
                    });
                }
                KeyCode::Char('x') => {
                    let Some(worker) = selected.clone().or_else(|| self.pool_worker_after(None, 0))
                    else {
                        self.set_status("Select a Worker before Dismiss".to_owned());
                        self.mode = Mode::Pool(PoolMode::View { selected });
                        return;
                    };
                    self.mode = Mode::ConfirmPoolDismiss(worker);
                }
                KeyCode::Char('g') => {
                    let Some(worker) = selected.clone().or_else(|| self.pool_worker_after(None, 0))
                    else {
                        self.set_status("Select a Worker before Rebase".to_owned());
                        self.mode = Mode::Pool(PoolMode::View { selected });
                        return;
                    };
                    self.mode = Mode::Pool(PoolMode::View {
                        selected: Some(worker.clone()),
                    });
                    self.start_worker_command(WorkspaceDispatchCommand::Rebase {
                        operation: 0,
                        worker,
                    });
                }
                KeyCode::Char('P') => {
                    let Some(worker) = selected.clone().or_else(|| self.pool_worker_after(None, 0))
                    else {
                        self.set_status(
                            "Select a Worker before opening its Pull Request".to_owned(),
                        );
                        self.mode = Mode::Pool(PoolMode::View { selected });
                        return;
                    };
                    self.mode = Mode::Pool(PoolMode::View {
                        selected: Some(worker.clone()),
                    });
                    self.start_worker_command(WorkspaceDispatchCommand::OpenPullRequest {
                        operation: 0,
                        worker,
                    });
                }
                KeyCode::Char('a') => self.discover_ready_tickets(),
                KeyCode::Char('o') => {
                    self.mode = Mode::Pool(PoolMode::ParentInput {
                        buffer: String::new(),
                    });
                }
                KeyCode::Char('l') => {
                    let worker = selected.clone().or_else(|| self.pool_worker_after(None, 0));
                    if let Some(worker) = worker {
                        self.start_worker_log(worker);
                    } else {
                        self.set_status("Select a Worker before opening its log".to_string());
                        self.mode = Mode::Pool(PoolMode::View { selected });
                    }
                }
                KeyCode::Char('r') => {
                    let capacity = self.pool_capacity().unwrap_or(0);
                    self.mode = Mode::Pool(PoolMode::Capacity {
                        buffer: capacity.to_string(),
                        selected,
                    });
                }
                KeyCode::Char('K') => {
                    let Some(worker) = selected.clone().or_else(|| self.pool_worker_after(None, 0))
                    else {
                        self.set_status("Select a Worker before Reset".to_owned());
                        self.mode = Mode::Pool(PoolMode::View { selected });
                        return;
                    };
                    self.mode = Mode::ConfirmPoolReset(worker);
                }
                KeyCode::Char('D') => self.mode = Mode::ConfirmPoolDestroy,
                _ => self.mode = Mode::Pool(PoolMode::View { selected }),
            },
            Mode::Pool(PoolMode::Capacity {
                mut buffer,
                selected,
            }) => match key.code {
                KeyCode::Esc => self.mode = Mode::Pool(PoolMode::View { selected }),
                KeyCode::Backspace => {
                    buffer.pop();
                    self.mode = Mode::Pool(PoolMode::Capacity { buffer, selected });
                }
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    buffer.push(c);
                    self.mode = Mode::Pool(PoolMode::Capacity { buffer, selected });
                }
                KeyCode::Enter => match buffer.parse::<usize>() {
                    Ok(capacity)
                        if self
                            .pool_capacity()
                            .is_some_and(|current| capacity < current) =>
                    {
                        self.mode = Mode::ConfirmPoolShrink(capacity, selected);
                    }
                    Ok(capacity) => {
                        self.mode = Mode::Pool(PoolMode::View { selected });
                        self.resize_pool(capacity);
                    }
                    Err(_) => {
                        self.set_status("Pool capacity must be a number".to_string());
                        self.mode = Mode::Pool(PoolMode::Capacity { buffer, selected });
                    }
                },
                _ => self.mode = Mode::Pool(PoolMode::Capacity { buffer, selected }),
            },
            Mode::Pool(PoolMode::AliasInput { worker, mut buffer }) => match key.code {
                KeyCode::Esc => {
                    self.mode = Mode::Pool(PoolMode::View {
                        selected: Some(worker),
                    });
                }
                KeyCode::Backspace => {
                    buffer.pop();
                    self.mode = Mode::Pool(PoolMode::AliasInput { worker, buffer });
                }
                KeyCode::Char(c) if !c.is_control() => {
                    buffer.push(c);
                    self.mode = Mode::Pool(PoolMode::AliasInput { worker, buffer });
                }
                KeyCode::Enter => {
                    self.mode = Mode::Pool(PoolMode::View {
                        selected: Some(worker.clone()),
                    });
                    self.set_worker_alias(worker, buffer);
                }
                _ => self.mode = Mode::Pool(PoolMode::AliasInput { worker, buffer }),
            },
            Mode::Pool(PoolMode::SendInput { worker, mut buffer }) => match key.code {
                KeyCode::Esc => {
                    self.mode = Mode::Pool(PoolMode::View {
                        selected: Some(worker),
                    })
                }
                KeyCode::Backspace => {
                    buffer.pop();
                    self.mode = Mode::Pool(PoolMode::SendInput { worker, buffer });
                }
                KeyCode::Char(c) if !c.is_control() => {
                    buffer.push(c);
                    self.mode = Mode::Pool(PoolMode::SendInput { worker, buffer });
                }
                KeyCode::Enter if !buffer.trim().is_empty() => {
                    self.mode = Mode::Pool(PoolMode::View {
                        selected: Some(worker.clone()),
                    });
                    self.send_to_worker(worker, buffer);
                }
                KeyCode::Enter => {
                    self.set_status("Send prompt cannot be empty".to_owned());
                    self.mode = Mode::Pool(PoolMode::SendInput { worker, buffer });
                }
                _ => self.mode = Mode::Pool(PoolMode::SendInput { worker, buffer }),
            },
            Mode::Pool(PoolMode::TicketInput {
                mut buffer,
                selected,
            }) => match key.code {
                KeyCode::Esc => self.mode = Mode::Pool(PoolMode::View { selected }),
                KeyCode::Backspace => {
                    buffer.pop();
                    self.mode = Mode::Pool(PoolMode::TicketInput { buffer, selected });
                }
                KeyCode::Char(c)
                    if c.is_ascii_alphanumeric()
                        || c == '-'
                        || c == '_'
                        || c.is_ascii_whitespace() =>
                {
                    buffer.push(c);
                    self.mode = Mode::Pool(PoolMode::TicketInput { buffer, selected });
                }
                KeyCode::Enter => {
                    let tickets = buffer
                        .split_whitespace()
                        .map(str::to_owned)
                        .collect::<Vec<_>>();
                    if tickets.is_empty() {
                        self.set_status("Ticket ID cannot be empty".to_string());
                        self.mode = Mode::Pool(PoolMode::TicketInput { buffer, selected });
                    } else if tickets.len() == 1
                        && selected
                            .as_deref()
                            .is_none_or(|worker| !self.worker_is_idle(worker))
                    {
                        self.set_status("Select an idle Worker before Dispatch".to_string());
                        self.mode = Mode::Pool(PoolMode::TicketInput { buffer, selected });
                    } else {
                        self.mode = Mode::Pool(PoolMode::DispatchPreview { tickets, selected });
                    }
                }
                _ => self.mode = Mode::Pool(PoolMode::TicketInput { buffer, selected }),
            },
            Mode::Pool(PoolMode::DispatchPreview { tickets, selected }) => match key.code {
                KeyCode::Esc => self.mode = Mode::Pool(PoolMode::View { selected }),
                KeyCode::Enter => {
                    let worker = (tickets.len() == 1).then(|| selected.clone()).flatten();
                    self.mode = Mode::Pool(PoolMode::View { selected });
                    self.dispatch_tickets(tickets, worker);
                }
                _ => self.mode = Mode::Pool(PoolMode::DispatchPreview { tickets, selected }),
            },
            Mode::Pool(PoolMode::ReadyPreview { selected }) => match key.code {
                KeyCode::Esc => self.mode = Mode::Pool(PoolMode::View { selected }),
                KeyCode::Enter => {
                    let tickets = self
                        .ready_tickets
                        .as_ref()
                        .map(|ready| {
                            ready
                                .tickets()
                                .iter()
                                .map(|ticket| ticket.id().to_owned())
                                .collect::<Vec<String>>()
                        })
                        .unwrap_or_default();
                    self.mode = Mode::Pool(PoolMode::View { selected });
                    if tickets.is_empty() {
                        self.set_status("No Ready Tickets to Dispatch".to_string());
                    } else {
                        self.dispatch_tickets(tickets, None);
                    }
                }
                _ => self.mode = Mode::Pool(PoolMode::ReadyPreview { selected }),
            },
            Mode::Pool(PoolMode::ParentInput { mut buffer }) => match key.code {
                KeyCode::Esc => self.mode = Mode::Pool(PoolMode::View { selected: None }),
                KeyCode::Backspace => {
                    buffer.pop();
                    self.mode = Mode::Pool(PoolMode::ParentInput { buffer });
                }
                KeyCode::Char(c) if c.is_ascii_alphanumeric() || c == '-' || c == '_' => {
                    buffer.push(c);
                    self.mode = Mode::Pool(PoolMode::ParentInput { buffer });
                }
                KeyCode::Enter if !buffer.trim().is_empty() => {
                    self.mode = Mode::Pool(PoolMode::View { selected: None });
                    self.orchestrate_parent(buffer);
                }
                KeyCode::Enter => {
                    self.set_status("Parent Ticket ID cannot be empty".to_string());
                    self.mode = Mode::Pool(PoolMode::ParentInput { buffer });
                }
                _ => self.mode = Mode::Pool(PoolMode::ParentInput { buffer }),
            },
            Mode::Pool(PoolMode::LogDetail { worker }) => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.stop_worker_log();
                    self.mode = Mode::Pool(PoolMode::View {
                        selected: Some(worker),
                    });
                }
                _ => self.mode = Mode::Pool(PoolMode::LogDetail { worker }),
            },
            Mode::Pool(PoolMode::ConfirmDispatchCapacity {
                tickets,
                worker,
                shortage,
            }) => self.on_key_confirm_dispatch_capacity(key, tickets, worker, shortage),
            other => self.mode = other,
        }
    }

    fn worker_is_available_for_action(&self, worker: &str) -> bool {
        self.worker_pool
            .as_ref()
            .and_then(|snapshot| snapshot.worker(worker))
            .is_some_and(|worker| worker.status() != WorkerStatus::Busy)
    }

    fn send_to_worker(&mut self, worker: String, prompt: String) {
        let operation = self.begin_dispatch();
        self.pin_status(format!("sending to {worker}..."));
        self.dispatch.submit(WorkspaceDispatchCommand::Send {
            operation,
            worker,
            prompt,
        });
    }

    fn review_worker(&mut self, worker: String) {
        let operation = self.begin_dispatch();
        self.pin_status(format!("starting review for {worker}..."));
        self.dispatch
            .submit(WorkspaceDispatchCommand::Review { operation, worker });
    }

    fn start_worker_command(&mut self, command: WorkspaceDispatchCommand) {
        let operation = self.begin_dispatch();
        let command = match command {
            WorkspaceDispatchCommand::Rebase { worker, .. } => {
                self.pin_status("rebasing Worker...".to_owned());
                WorkspaceDispatchCommand::Rebase { operation, worker }
            }
            WorkspaceDispatchCommand::OpenPullRequest { worker, .. } => {
                self.pin_status("opening Worker Pull Request...".to_owned());
                WorkspaceDispatchCommand::OpenPullRequest { operation, worker }
            }
            WorkspaceDispatchCommand::SetAlias { worker, alias, .. } => {
                self.pin_status("saving Worker alias...".to_owned());
                WorkspaceDispatchCommand::SetAlias {
                    operation,
                    worker,
                    alias,
                }
            }
            WorkspaceDispatchCommand::Dismiss { worker, .. } => {
                self.pin_status("dismissing Worker...".to_owned());
                WorkspaceDispatchCommand::Dismiss { operation, worker }
            }
            _ => return,
        };
        self.dispatch.submit(command);
    }

    fn set_worker_alias(&mut self, worker: String, alias: String) {
        self.start_worker_command(WorkspaceDispatchCommand::SetAlias {
            operation: 0,
            worker,
            alias,
        });
    }

    fn discover_ready_tickets(&mut self) {
        let operation = self.begin_dispatch();
        self.pin_status("discovering Ready Tickets...".to_string());
        self.dispatch
            .submit(WorkspaceDispatchCommand::DiscoverReady {
                operation,
                label: "ready-for-agent".to_owned(),
            });
    }

    fn on_key_confirm_dispatch_capacity(
        &mut self,
        key: KeyEvent,
        tickets: Vec<String>,
        worker: Option<String>,
        shortage: crate::workspace_dispatch::DispatchCapacityShortage,
    ) {
        let Some(operation) = self.active_operation else {
            self.mode = Mode::Pool(PoolMode::View { selected: worker });
            return;
        };
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.pin_status(format!("growing Pool by {} Worker(s)...", shortage.gap()));
                self.dispatch
                    .submit(WorkspaceDispatchCommand::DispatchWithApprovedGrowth {
                        operation,
                        tickets,
                        worker,
                        additional: shortage.gap(),
                    });
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                self.pin_status("dispatching available Tickets...".to_string());
                self.dispatch
                    .submit(WorkspaceDispatchCommand::DispatchUseAvailable {
                        operation,
                        tickets,
                        worker,
                    });
            }
            KeyCode::Esc => {
                self.active_operation = None;
                self.mode = Mode::Pool(PoolMode::View { selected: worker });
                self.set_status("Dispatch cancelled".to_string());
            }
            _ => {
                self.mode = Mode::Pool(PoolMode::ConfirmDispatchCapacity {
                    tickets,
                    worker,
                    shortage,
                });
            }
        }
    }

    fn on_key_confirm_pool_shrink(&mut self, key: KeyEvent) {
        let Mode::ConfirmPoolShrink(capacity, selected) = &self.mode else {
            return;
        };
        let capacity = *capacity;
        let selected = selected.clone();
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.mode = Mode::Pool(PoolMode::View { selected });
                self.resize_pool(capacity);
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.mode = Mode::Pool(PoolMode::View { selected });
            }
            _ => {}
        }
    }

    fn on_key_confirm_pool_reset(&mut self, key: KeyEvent) {
        let Mode::ConfirmPoolReset(worker) = &self.mode else {
            return;
        };
        let worker = worker.clone();
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.mode = Mode::Pool(PoolMode::View {
                    selected: Some(worker.clone()),
                });
                let operation = self.begin_dispatch();
                self.worker_reset_operation = Some(operation);
                self.pin_status(format!("resetting Worker {worker}..."));
                self.dispatch
                    .submit(WorkspaceDispatchCommand::Reset { operation, worker });
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.mode = Mode::Pool(PoolMode::View {
                    selected: Some(worker),
                });
            }
            _ => {}
        }
    }

    fn on_key_confirm_pool_dismiss(&mut self, key: KeyEvent) {
        let Mode::ConfirmPoolDismiss(worker) = &self.mode else {
            return;
        };
        let worker = worker.clone();
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.mode = Mode::Pool(PoolMode::View {
                    selected: Some(worker.clone()),
                });
                self.start_worker_command(WorkspaceDispatchCommand::Dismiss {
                    operation: 0,
                    worker,
                });
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.mode = Mode::Pool(PoolMode::View {
                    selected: Some(worker),
                });
            }
            _ => {}
        }
    }

    fn on_key_confirm_pool_destroy(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.mode = Mode::Normal;
                self.destroy_pool();
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.mode = Mode::Pool(PoolMode::View { selected: None });
            }
            _ => {}
        }
    }

    fn dispatch_tickets(&mut self, tickets: Vec<String>, worker: Option<String>) {
        let operation = self.begin_dispatch();
        self.pin_status(format!("dispatching {} Ticket(s)...", tickets.len()));
        self.dispatch.submit(WorkspaceDispatchCommand::Dispatch {
            operation,
            tickets,
            worker,
        });
    }

    fn resize_pool(&mut self, capacity: usize) {
        let operation = self.begin_dispatch();
        self.pin_status(format!("resizing Worker Pool to {capacity}..."));
        self.dispatch.submit(WorkspaceDispatchCommand::Resize {
            operation,
            capacity,
        });
    }

    fn destroy_pool(&mut self) {
        let operation = self.begin_dispatch();
        self.pin_status("destroying Worker Pool...".to_string());
        self.dispatch
            .submit(WorkspaceDispatchCommand::Destroy { operation });
    }

    fn pool_capacity(&self) -> Option<usize> {
        self.worker_pool
            .as_ref()?
            .pool()
            .and_then(|pool| usize::try_from(pool.size()).ok())
    }

    fn worker_is_idle(&self, worker_id: &str) -> bool {
        self.worker_pool
            .as_ref()
            .and_then(|snapshot| {
                snapshot
                    .workers()
                    .iter()
                    .find(|worker| worker.worker_id().as_str() == worker_id)
            })
            .is_some_and(|worker| worker.status() == WorkerStatus::Idle)
    }

    fn pool_worker_after(&self, selected: Option<&str>, delta: isize) -> Option<String> {
        let workers = self.worker_pool.as_ref()?.workers();
        if workers.is_empty() {
            return None;
        }
        let current = selected
            .and_then(|id| {
                workers
                    .iter()
                    .position(|worker| worker.worker_id().as_str() == id)
            })
            .unwrap_or(0) as isize;
        let index = (current + delta).clamp(0, workers.len() as isize - 1) as usize;
        Some(workers[index].worker_id().to_string())
    }

    fn on_key_new_workspace(&mut self, key: KeyEvent) {
        let Mode::NewWorkspace(buf) = &mut self.mode else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let name = buf.clone();
                self.mode = Mode::Normal;
                self.create_workspace(&name);
            }
            KeyCode::Backspace => {
                buf.pop();
            }
            // Keep names to a safe, filesystem- and jj-friendly character set.
            KeyCode::Char(c) if c.is_alphanumeric() || c == '-' || c == '_' => buf.push(c),
            _ => {}
        }
    }

    fn on_key_confirm_delete(&mut self, key: KeyEvent) {
        let Mode::ConfirmDelete(name) = &self.mode else {
            return;
        };
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let name = name.clone();
                self.mode = Mode::Normal;
                self.delete_workspace(&name);
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.mode = Mode::Normal,
            _ => {}
        }
    }

    fn on_key_confirm_tidy(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.mode = Mode::Normal;
                self.tidy();
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.mode = Mode::Normal,
            _ => {}
        }
    }

    /// The workspace under the cursor, if any.
    fn selected_workspace(&self) -> Option<&Workspace> {
        self.list
            .selected()
            .and_then(|name| self.store.workspace(name))
    }

    fn refuse_if_configuring(&mut self, name: &str) -> bool {
        let configuring = self
            .pending_workspace
            .as_ref()
            .is_some_and(|pending| pending.name == name);
        if configuring {
            self.set_status(format!("workspace '{name}' is still configuring"));
        }
        configuring
    }

    fn refuse_while_configuring(&mut self) -> bool {
        let Some(pending) = self.pending_workspace.as_ref() else {
            return false;
        };
        let message = format!("workspace '{}' is still configuring", pending.name);
        self.set_status(message);
        true
    }

    /// `enter`/`o`: focus the selected workspace's tab if it exists, else open a
    /// new one. `focus` steals focus (`enter`); otherwise it opens in the
    /// background (`o`) - and when a background target already exists, we leave
    /// it untouched rather than raising it.
    fn open_selected(&mut self, focus: bool) {
        self.status = None;
        let Some(w) = self.selected_workspace().cloned() else {
            return;
        };
        if self.refuse_if_configuring(&w.name) {
            return;
        }
        let Some(path) = w.path.clone() else {
            self.set_status(format!("no path known for '{}'", w.name));
            return;
        };
        let result = if self.terminal.is_open(&w.name) {
            if focus {
                self.terminal.focus(&w.name)
            } else {
                Ok(())
            }
        } else {
            self.terminal.open(&w.name, &path, focus)
        };
        if let Err(e) = result {
            self.set_status(format!("open failed: {e}"));
        }
    }

    /// `d`: confirm before deleting; the default workspace is undeletable.
    fn begin_delete_selected(&mut self) {
        self.status = None;
        let Some(name) = self
            .selected_workspace()
            .map(|workspace| workspace.name.clone())
        else {
            return;
        };
        if name == store::DEFAULT_WORKSPACE {
            self.set_status("the default workspace cannot be deleted".to_string());
            return;
        }
        if self.refuse_if_configuring(&name) {
            return;
        }
        self.mode = Mode::ConfirmDelete(name);
    }

    /// Create a workspace through the lifecycle module, then present it in the
    /// terminal. Persistence and reconciliation stay behind the Store interface.
    fn create_workspace(&mut self, requested_name: &str) {
        let created = match self.store.create(requested_name) {
            Ok(created) => created,
            Err(error) => {
                self.set_status(format!("{error:#}"));
                return;
            }
        };
        self.after_store_changed();
        if self.workspace_config.on_create.is_empty() {
            self.open_created_workspace(created.name(), created.path());
            return;
        }

        let workspace = PendingWorkspace {
            name: created.name().to_string(),
            path: created.path().to_path_buf(),
        };
        self.pending_workspace = Some(workspace.clone());
        self.pin_status(format!("configuring '{}'...", workspace.name));

        let command = self.workspace_config.on_create.clone();
        let tx = self.tx.clone();
        let run_path = workspace.path.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || run_on_create(&command, &run_path))
                .await
                .map_err(|error| format!("{error:#}"))
                .and_then(|result| result.map_err(|error| format!("{error:#}")));
            let _ = tx.send(Msg::WorkspaceConfigured { workspace, result });
        });
    }

    fn on_workspace_configured(&mut self, workspace: PendingWorkspace, result: Result<(), String>) {
        if self.pending_workspace.as_ref() != Some(&workspace) {
            return;
        }
        self.pending_workspace = None;
        let PendingWorkspace { name, path } = workspace;

        if let Err(error) = result {
            self.set_status(format!("created '{name}', on-create failed: {error}"));
            return;
        }
        let still_exists = self
            .store
            .workspace(&name)
            .and_then(|workspace| workspace.path.as_deref())
            == Some(path.as_path())
            && path.is_dir();
        if !still_exists {
            self.set_status(format!(
                "created '{name}', on-create finished after the workspace disappeared"
            ));
            return;
        }
        self.open_created_workspace(&name, &path);
    }

    fn open_created_workspace(&mut self, name: &str, path: &std::path::Path) {
        match self.terminal.open(name, path, true) {
            Ok(()) => self.set_status(format!("created '{name}'")),
            Err(error) => self.set_status(format!("created '{name}', tab failed: {error}")),
        }
    }

    /// Close a workspace's terminal presentation before deleting it through the
    /// lifecycle module, preserving the existing best-effort terminal ordering.
    fn delete_workspace(&mut self, name: &str) {
        let _ = self.terminal.close(name); // best-effort; jj is the source of truth
        if let Err(error) = self.store.delete(name) {
            self.set_status(format!("{error:#}"));
            return;
        }
        self.after_store_changed();
        self.set_status(format!("deleted '{name}'"));
    }

    /// `t`: reset idle, empty, undescribed workspace working-copies onto latest
    /// `trunk()`. Non-destructive (workspaces with real work are untouched), so it
    /// runs without confirmation; the poller refreshes each row's `behind` count.
    fn tidyws(&mut self) {
        if self.refuse_while_configuring() {
            return;
        }
        let msg = match self.jj.tidyws() {
            Ok(0) => "tidyws: nothing to reset".to_string(),
            Ok(n) => format!("tidyws: reset {n} workspace(s) onto trunk"),
            Err(e) => format!("tidyws failed: {e}"),
        };
        self.set_status(msg);
        self.reload();
    }

    /// `T`: confirm before the destructive `tidy` sweep.
    fn begin_tidy(&mut self) {
        if self.refuse_while_configuring() {
            return;
        }
        self.status = None;
        self.mode = Mode::ConfirmTidy;
    }

    /// Abandon junk empties across the repo (mutable, empty, undescribed,
    /// unbookmarked, untagged, never `@`), after confirmation.
    fn tidy(&mut self) {
        let msg = match self.jj.tidy() {
            Ok(0) => "tidy: nothing to abandon".to_string(),
            Ok(n) => format!("tidy: abandoned {n} junk empty change(s)"),
            Err(e) => format!("tidy failed: {e}"),
        };
        self.set_status(msg);
        self.reload();
    }

    /// `r`: lift the selected workspace's stack onto trunk (local rebase, no
    /// push) - the remedy for a `behind` workspace, empty or not.
    fn lift_selected(&mut self) {
        let Some(w) = self.selected_workspace().cloned() else {
            return;
        };
        if self.refuse_if_configuring(&w.name) {
            return;
        }
        let msg = match self.jj.lift(&w.name) {
            Ok(true) => format!("lifted {} onto trunk", w.name),
            Ok(false) => format!("{}: nothing to lift", w.name),
            Err(e) => format!("lift failed: {e}"),
        };
        self.set_status(msg);
        self.reload();
    }

    /// `R`: lift every workspace's stack onto trunk in one rebase.
    fn lift_all(&mut self) {
        if self.refuse_while_configuring() {
            return;
        }
        let msg = match self.jj.lift_all() {
            Ok(true) => "lifted all workspaces onto trunk".to_string(),
            Ok(false) => "nothing to lift".to_string(),
            Err(e) => format!("lift failed: {e}"),
        };
        self.set_status(msg);
        self.reload();
    }

    /// `u`: fetch from the git remote on a background task (network-bound - it
    /// must never block the render loop, so unlike the local verbs it cannot go
    /// through the synchronous `Jj` trait). This is how remote-only changes - a
    /// PR merged on GitHub, its head branch deleted - reach the work rows
    /// without running a full forge. The outcome lands as [`Msg::Fetched`].
    fn fetch(&mut self) {
        if self.fetching {
            return;
        }
        self.fetching = true;
        self.pin_status("fetching…".to_string());
        let tx = self.tx.clone();
        let repo_root = self.store.repo_root().to_path_buf();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || jj::fetch(&repo_root))
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!(e)));
            let _ = tx.send(Msg::Fetched(result.map_err(|e| format!("{e:#}"))));
        });
    }

    /// Fold the background fetch's outcome into the footer, then reload. The
    /// explicit reload matters for the no-op case: a fetch that brought nothing
    /// changes no files, so the watcher stays silent and this is the only
    /// refresh the row gets.
    fn on_fetched(&mut self, result: Result<(), String>) {
        self.fetching = false;
        self.set_status(match result {
            Ok(()) => "fetched".to_string(),
            Err(e) => format!("fetch failed: {e}"),
        });
        self.reload();
    }

    /// `f`: forge the selected workspace.
    fn forge_selected(&mut self) {
        if let Some(w) = self.selected_workspace().cloned() {
            if self.refuse_if_configuring(&w.name) {
                return;
            }
            self.start_forge(vec![w]);
        }
    }

    /// `g`: forge the default workspace.
    fn forge_default(&mut self) {
        if let Some(w) = self
            .store
            .workspaces()
            .iter()
            .find(|w| w.name == store::DEFAULT_WORKSPACE)
            .cloned()
        {
            self.start_forge(vec![w]);
        }
    }

    /// `F`: forge every eligible workspace, sequentially (in one background run).
    fn forge_all(&mut self) {
        if self.refuse_while_configuring() {
            return;
        }
        let all: Vec<Workspace> = self.store.workspaces().to_vec();
        self.start_forge(all);
    }

    /// Spawn a background forge run for the given workspaces. Workspaces already
    /// forging, or with no known path (the revsets need a working dir), are
    /// skipped. The task streams progress back as [`Msg::Forge`].
    fn start_forge(&mut self, workspaces: Vec<Workspace>) {
        self.status = None;
        let mut targets = Vec::new();
        let mut skipped_no_path = None;
        for w in workspaces {
            if self
                .forge_progress
                .get(&w.name)
                .is_some_and(forge::Progress::active)
            {
                continue; // already forging
            }
            match &w.path {
                Some(dir) => targets.push(Target::new(w.name.clone(), dir.clone())),
                None => skipped_no_path = Some(w.name.clone()),
            }
        }
        if targets.is_empty() {
            self.set_status(match skipped_no_path {
                Some(name) => format!("no path known for '{name}'"),
                None => "nothing to forge".to_string(),
            });
            return;
        }
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

    /// The ordered selectable workspace names for the current classification.
    fn selectable_names(&self) -> Vec<String> {
        self.list.selectable(&self.classified())
    }

    fn move_selection(&mut self, delta: isize) {
        let names = self.selectable_names();
        self.list.move_selection(&names, delta);
    }

    /// Re-reconcile from disk; the selection follows its workspace by name.
    fn reload(&mut self) {
        self.store.reload();
        self.after_store_changed();
    }

    fn after_store_changed(&mut self) {
        self.ensure_selection();
        // The cache/op-log changed on disk (new commits, fetch, workspace edits);
        // refresh the graph if a graph-bearing view is open.
        self.refresh_graph_if_visible();
    }

    /// Point the selection at a real, currently-selectable workspace, falling
    /// back to the first one when the current target is gone or hidden.
    fn ensure_selection(&mut self) {
        let names = self.selectable_names();
        self.list.ensure_selection(&names);
    }

    /// Workspaces paired with their derived Attention, grouped needs-you ->
    /// working -> ready-to-forge -> idle, sorted by name within each group.
    fn classified(&self) -> Vec<(Attention, &Workspace)> {
        let mut v: Vec<(Attention, &Workspace)> = self
            .store
            .workspaces()
            .iter()
            .map(|w| {
                (
                    attention::derive(self.agent_state(w), self.work_state(w)),
                    w,
                )
            })
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
        v
    }

    /// Render the Attention-grouped workspace list plus a header and footer.
    pub fn render(&mut self, frame: &mut Frame) {
        self.ensure_selection();

        let base_mode = match &self.mode {
            Mode::Help(previous) => previous.as_ref(),
            mode => mode,
        };
        if matches!(base_mode, Mode::Detail(_)) {
            self.render_detail(frame);
            return;
        }
        if matches!(base_mode, Mode::Graph(_)) {
            self.render_graph_world(frame);
            return;
        }
        if matches!(base_mode, Mode::Pool(_)) {
            self.render_pool(frame);
            if matches!(self.mode, Mode::Help(_)) {
                self.render_help(frame, POOL_BINDINGS);
            }
            return;
        }

        let [header, body, footer] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .horizontal_margin(2)
        .areas(frame.area());

        let title = match &self.worker_pool {
            Some(snapshot) if snapshot.diagnostics().is_empty() => format!(
                "jjfx - {} workspace(s)  [wsg pool]",
                self.store.workspaces().len()
            ),
            Some(snapshot) => format!(
                "jjfx - {} workspace(s)  [wsg pool, {} issue(s)]",
                self.store.workspaces().len(),
                snapshot.diagnostics().len()
            ),
            None => format!("jjfx - {} workspace(s)", self.store.workspaces().len()),
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                title,
                Style::default().add_modifier(Modifier::BOLD),
            )),
            header,
        );

        // The optional world-graph pane under the list: content-sized, capped at
        // half the body, and dropped entirely when the body is too short to
        // split usefully (the list stays readable - graceful degradation).
        let world = self
            .world
            .is_some()
            .then(|| self.world_lines(body.width))
            .filter(|_| body.height / 2 >= MIN_WORLD_PANE_HEIGHT)
            .map(|lines| {
                let h = (lines.len() as u16).saturating_add(2).min(body.height / 2);
                let [list_area, world_area] =
                    Layout::vertical([Constraint::Min(0), Constraint::Length(h)]).areas(body);
                (lines, list_area, world_area)
            });
        let list_area = world.as_ref().map_or(body, |(_, list_area, _)| *list_area);

        // Build list items from the grouped rows, tracking which item index the
        // selected workspace lands on so the highlight follows it.
        let classified = self.classified();
        let workers = self
            .worker_pool
            .as_ref()
            .map_or(&[][..], WorkerPoolSnapshot::workers);
        let rows = self.list.presentation_rows(&classified, workers);
        let mut cursor = None;
        let items: Vec<ListItem> = rows
            .iter()
            .enumerate()
            .map(|(i, row)| match row {
                PresentationRow::Header(att, count) => self.header_item(*att, *count),
                PresentationRow::Ws {
                    workspace,
                    attention,
                    worker,
                } => {
                    let is_selected = self.list.selected() == Some(workspace.name.as_str());
                    if is_selected {
                        cursor = Some(i);
                    }
                    self.workspace_item(
                        workspace,
                        *attention,
                        is_selected,
                        *worker,
                        list_area.width,
                    )
                }
            })
            .collect();
        drop(rows);
        drop(classified);

        self.list.render_body(frame, list_area, items, cursor);

        if let Some((lines, _, world_area)) = world
            && let Some(vp) = &mut self.world
        {
            vp.resize(world_area.height.saturating_sub(2), lines.len() as u16);
            frame.render_widget(
                Paragraph::new(lines)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(pane_border(false))
                            .title(" world "),
                    )
                    .scroll((vp.scroll(), 0)),
                world_area,
            );
        }

        frame.render_widget(self.footer(), footer);

        if matches!(self.mode, Mode::Help(_)) {
            self.render_help(frame, NORMAL_BINDINGS);
        }
    }

    fn pool_mode(&self) -> Option<&PoolMode> {
        match &self.mode {
            Mode::Pool(mode) => Some(mode),
            Mode::Help(previous) => match previous.as_ref() {
                Mode::Pool(mode) => Some(mode),
                _ => None,
            },
            _ => None,
        }
    }

    fn pool_selected(&self) -> Option<&str> {
        match self.pool_mode()? {
            PoolMode::View { selected }
            | PoolMode::Capacity { selected, .. }
            | PoolMode::TicketInput { selected, .. }
            | PoolMode::DispatchPreview { selected, .. }
            | PoolMode::ReadyPreview { selected }
            | PoolMode::ConfirmDispatchCapacity {
                worker: selected, ..
            } => selected.as_deref(),
            PoolMode::SendInput { worker, .. }
            | PoolMode::AliasInput { worker, .. }
            | PoolMode::LogDetail { worker } => Some(worker),
            PoolMode::ParentInput { .. } => None,
        }
    }

    fn render_pool(&mut self, frame: &mut Frame) {
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .horizontal_margin(2)
        .areas(frame.area());
        frame.render_widget(
            Paragraph::new(Span::styled(
                "Worker Pool  [p management]",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            header,
        );

        let mut lines = Vec::new();
        if let Some(snapshot) = &self.worker_pool {
            let capacity = snapshot
                .pool()
                .and_then(|pool| usize::try_from(pool.size()).ok())
                .map_or_else(|| "missing".to_string(), |capacity| capacity.to_string());
            lines.push(Line::from(format!(" capacity: {capacity}")));
            if snapshot.workers().is_empty() {
                lines.push(dim_line(" (no Workers)"));
            }
            let selected = self.pool_selected();
            for worker in snapshot.workers() {
                let marker = if selected == Some(worker.worker_id().as_str()) {
                    "▸"
                } else {
                    "·"
                };
                let line = if body.width < 55 {
                    format!(
                        " {marker} {}  {}  ticket:{}",
                        worker.worker_id(),
                        worker.status().as_str(),
                        worker.ticket().unwrap_or("-")
                    )
                } else {
                    format!(
                        " {marker} {}  {:<7} {}  {}",
                        worker.worker_id(),
                        worker.status().as_str(),
                        worker.alias(),
                        worker.workspace()
                    )
                };
                lines.push(Line::from(line));
            }
            for diagnostic in snapshot.diagnostics() {
                lines.push(Line::from(Span::styled(
                    format!(" ! {}", diagnostic.message()),
                    Style::default().fg(Color::Yellow),
                )));
            }
            if let Some(progress) = &self.group_progress {
                let counts = progress.counts();
                let terminal = if progress.is_terminal() {
                    " terminal"
                } else {
                    ""
                };
                lines.push(dim_line(&format!(
                    " Dispatch Group {}  waves: {}  done: {}  failed: {}  skipped: {}{}",
                    progress.parent(),
                    progress.maximum_wave(),
                    counts.done(),
                    counts.failed(),
                    counts.skipped(),
                    terminal
                )));
                if !progress.ready().is_empty() {
                    lines.push(Line::from(format!(
                        "  ready: {}",
                        progress.ready().join(", ")
                    )));
                }
                for issue in progress.issues() {
                    let blockers = if issue.blockers().is_empty() {
                        String::new()
                    } else {
                        format!("  blocked by {}", issue.blockers().join(", "))
                    };
                    let retry = if issue.retries() > 0 {
                        format!("  retry {}", issue.retries())
                    } else {
                        String::new()
                    };
                    lines.push(Line::from(format!(
                        "  wave {}  {:<10} {}  {} -> {}{}{}",
                        issue.wave(),
                        issue.status().as_str(),
                        issue.ticket(),
                        issue.title(),
                        issue.worker().unwrap_or("unassigned"),
                        retry,
                        blockers
                    )));
                }
            }
            match &self.mode {
                Mode::Pool(PoolMode::DispatchPreview { tickets, selected }) => {
                    lines.push(dim_line(" Dispatch preview:"));
                    for ticket in tickets {
                        let target = if tickets.len() == 1 {
                            selected.as_deref().unwrap_or("no Worker")
                        } else {
                            "first idle Worker"
                        };
                        lines.push(Line::from(format!("  ? {ticket} -> {target}")));
                    }
                }
                Mode::Pool(PoolMode::ReadyPreview { .. }) => {
                    if let Some(ready) = &self.ready_tickets {
                        lines.push(dim_line(" Ready Ticket preview:"));
                        for ticket in ready.tickets() {
                            lines.push(Line::from(format!(
                                "  ? {}  {}",
                                ticket.id(),
                                ticket.title()
                            )));
                        }
                        for diagnostic in ready.diagnostics() {
                            lines.push(Line::from(Span::styled(
                                format!(" ! {diagnostic}"),
                                Style::default().fg(Color::Yellow),
                            )));
                        }
                    }
                }
                _ => {}
            }
            if let Some(session) = &self.worker_session {
                let session_text = match session.session() {
                    AgentSessionResolution::Resumed { session_id } => {
                        format!("resumed session {session_id}")
                    }
                    AgentSessionResolution::Fresh { reason } => {
                        format!("fresh session ({reason})")
                    }
                };
                lines.push(dim_line(&format!(
                    " {} {}  runtime: {}  PID {}",
                    session.action().as_str(),
                    session.worker(),
                    session.runtime().as_str(),
                    session.pid()
                )));
                lines.push(Line::from(format!("  {session_text}")));
            }
            if let Some(result) = &self.worker_command_result {
                lines.push(dim_line(&format!(" Worker action: {}", result.notice())));
            }
            if let Some(result) = &self.dispatch_result {
                lines.push(dim_line(" Dispatch outcomes:"));
                for outcome in result.outcomes() {
                    let line = if outcome.succeeded() {
                        format!(
                            "  ✓ {}  {} -> {} (PID {})",
                            outcome.ticket(),
                            outcome.title(),
                            outcome.worker().unwrap_or("?"),
                            outcome.pid().unwrap_or_default()
                        )
                    } else {
                        format!(
                            "  ✗ {} [{}] {}",
                            outcome.ticket(),
                            outcome.phase().unwrap_or("unknown"),
                            outcome.detail().unwrap_or("Dispatch failed")
                        )
                    };
                    lines.push(Line::from(line));
                }
            }
            if let Mode::Pool(PoolMode::LogDetail { worker }) = &self.mode {
                lines.push(dim_line(&format!(" Worker log: {worker}")));
                if let Some(error) = &self.worker_log_error {
                    lines.push(Line::from(Span::styled(
                        format!(" ! {error}"),
                        Style::default().fg(Color::Yellow),
                    )));
                } else if let Some(snapshot) = &self.worker_log {
                    lines.push(Line::from(format!(
                        " runtime: {}  worker: {}",
                        snapshot.runtime().as_str(),
                        snapshot.worker()
                    )));
                    match snapshot.activity() {
                        Some(activity) => lines.push(Line::from(format!(
                            " activity: {}",
                            worker_activity_line(activity)
                        ))),
                        None => lines.push(dim_line(" no recognized activity yet")),
                    }
                    if let Some(result) = snapshot.result() {
                        lines.push(Line::from(format!(
                            " result: {}",
                            worker_result_line(result)
                        )));
                    }
                } else {
                    lines.push(dim_line(" loading latest activity..."));
                }
            }
        } else {
            lines.push(dim_line(" loading Worker Pool..."));
        }
        frame.render_widget(
            Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" pool ")),
            body,
        );
        frame.render_widget(self.footer(), footer);
    }

    /// The `?` overlay: a centered, bordered box listing every binding
    /// (label-left, key-right) drawn over a dimmed copy of the list behind it.
    fn render_help(&self, frame: &mut Frame, bindings: &[(&str, &str)]) {
        let label_w = bindings
            .iter()
            .map(|(label, _)| label.chars().count())
            .max()
            .unwrap_or(0);
        let key_w = bindings
            .iter()
            .map(|(_, key)| key.chars().count())
            .max()
            .unwrap_or(0);

        let lines: Vec<Line> = bindings
            .iter()
            .map(|(label, key)| {
                Line::from(vec![
                    Span::raw(format!(" {label:<label_w$}")),
                    Span::raw("   "),
                    Span::styled(
                        format!("{key:>key_w$} "),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ])
            })
            .collect();

        // Inner content is " label   key " plus the two borders.
        let width = (label_w + key_w + 5) as u16 + 2;
        let height = bindings.len() as u16 + 2;
        let area = centered_rect(frame.area(), width, height);

        // Dim everything already drawn so the popup reads as the foreground,
        // then punch the popup area clear before drawing it.
        let full = frame.area();
        let buf = frame.buffer_mut();
        for y in full.top()..full.bottom() {
            for x in full.left()..full.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_style(Style::default().add_modifier(Modifier::DIM));
                }
            }
        }

        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Keybindings "),
            ),
            area,
        );
    }

    /// The full-screen diff detail: a title line, a horizontal split of the
    /// changed-file list (with +/- magnitude bars) and the selected file's
    /// highlighted diff, and a focus-sensitive footer.
    fn render_detail(&mut self, frame: &mut Frame) {
        // Disjoint borrows: the diff panes need `&mut Detail`, the graph strip
        // needs `&graph`. Split `self` into its fields so both are live at once.
        let Self { mode, graph, .. } = self;
        let Mode::Detail(d) = mode else {
            return;
        };

        let [title, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .horizontal_margin(2)
        .areas(frame.area());

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("diff  ", Style::default().add_modifier(Modifier::DIM)),
                Span::styled(
                    d.workspace().to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled("  from trunk", Style::default().add_modifier(Modifier::DIM)),
            ])),
            title,
        );

        // The per-workspace graph strip rides on the right, but only when there is
        // room for files + a usable diff + the strip; otherwise it is dropped so a
        // narrow terminal keeps a readable diff (graceful degradation, AC 4).
        let show_graph = body.width >= FILES_PANE_WIDTH + GRAPH_PANE_WIDTH + MIN_DIFF_WIDTH;
        if show_graph {
            let [files_area, diff_area, graph_area] = Layout::horizontal([
                Constraint::Length(FILES_PANE_WIDTH),
                Constraint::Min(0),
                Constraint::Length(GRAPH_PANE_WIDTH),
            ])
            .areas(body);
            d.render_files(frame, files_area);
            d.render_diff(frame, diff_area);
            render_graph_pane(frame, graph.as_ref(), d.workspace(), graph_area);
        } else {
            let [files_area, diff_area] =
                Layout::horizontal([Constraint::Length(FILES_PANE_WIDTH), Constraint::Min(0)])
                    .areas(body);
            d.render_files(frame, files_area);
            d.render_diff(frame, diff_area);
        }

        frame.render_widget(d.footer(), footer);
    }

    /// The full-screen world graph: a title, the bordered graph (trunk plus every
    /// workspace's chain, the selected chain highlighted), and a scroll footer.
    fn render_graph_world(&mut self, frame: &mut Frame) {
        let [title, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .horizontal_margin(2)
        .areas(frame.area());

        // Build the lines before mutably borrowing the mode's scroll state.
        let lines = self.world_lines(body.width);
        let Mode::Graph(g) = &mut self.mode else {
            return;
        };

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("graph  ", Style::default().add_modifier(Modifier::DIM)),
                Span::styled("world", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(
                    "  every workspace, jj log shaped",
                    Style::default().add_modifier(Modifier::DIM),
                ),
            ])),
            title,
        );

        g.resize(body.height.saturating_sub(2), lines.len() as u16);

        frame.render_widget(
            Paragraph::new(lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(pane_border(true))
                        .title(" commit graph "),
                )
                .scroll((g.scroll(), 0)),
            body,
        );

        frame.render_widget(
            Paragraph::new(Span::styled(
                " j/k scroll · PgUp/PgDn page · g/G top/bottom · esc back ",
                Style::default().add_modifier(Modifier::DIM),
            )),
            footer,
        );
    }

    /// The world graph's rendered lines - the repo DAG laid out like `jj log`,
    /// the selected chain highlighted - with loading/empty placeholders. Shared
    /// by the full-screen view and the inline home pane.
    fn world_lines(&self, width: u16) -> Vec<Line<'static>> {
        match self.graph.as_ref() {
            Some(g) if !g.chains.is_empty() => {
                world_graph_lines(g, self.list.selected(), now_millis(), width)
            }
            Some(_) => vec![dim_line(" (no workspaces)")],
            None => vec![dim_line(" loading…")],
        }
    }

    /// A group-header row: the Attention heading, count, and a fold hint for idle.
    fn header_item(&self, att: Attention, count: usize) -> ListItem<'static> {
        let mut text = format!("{} ({count})", att.heading());
        if att == Attention::Idle {
            text.push_str(if self.list.idle_collapsed() {
                "  [c: expand]"
            } else {
                "  [c: fold]"
            });
        }
        ListItem::new(Line::from(Span::styled(
            text,
            Style::default()
                .fg(attention_color(att))
                .add_modifier(Modifier::BOLD),
        )))
    }

    /// A workspace row: Attention badge, then the two lifecycle axes, then name
    /// and path.
    fn workspace_item(
        &self,
        w: &Workspace,
        att: Attention,
        selected: bool,
        worker: Option<&WorkerSnapshot>,
        width: u16,
    ) -> ListItem<'static> {
        let agent = self.agent_of(w);
        let work = self.work_state(w);
        let behind = self.behind(w);
        // How far behind trunk: dimmed unless it is far enough to warrant tidyws.
        let behind_label = if behind > 0 {
            format!("↓{behind}")
        } else {
            String::new()
        };
        let path = w
            .path
            .as_deref()
            .map(display_path)
            .unwrap_or_else(|| "(path unknown - not in ws-cache)".to_string());
        // A dim bullet marks every row; the selected row's bullet brightens as the
        // only structural cue, keeping the line otherwise calm.
        let bullet = if selected {
            Span::styled("▸ ", Style::default().fg(Color::White))
        } else {
            Span::styled("· ", Style::default().fg(Color::DarkGray))
        };
        let mut spans = vec![
            bullet,
            Span::styled(
                // Widest heading ("ready to forge") is 14 chars; pad past it so
                // the following columns align across every row.
                format!("{:<15}", att.heading()),
                Style::default().fg(attention_color(att)),
            ),
            agent_glyph(agent, self.tick),
            Span::styled(
                format!("{:<11}", agent_label(agent)),
                Style::default().fg(agent_color(agent)),
            ),
        ];
        // While a forge is running, its live pipeline takes the work column;
        // otherwise the work label shows there.
        match self.forge_progress.get(&w.name) {
            Some(progress) => spans.extend(forge_spans(progress)),
            None => spans.push(Span::styled(
                format!("{:<16}", work.label()),
                Style::default().fg(work_color(work)),
            )),
        }
        spans.push(Span::styled(
            format!("{behind_label:<5}"),
            Style::default().fg(behind_color(behind)),
        ));
        // The name is boxed (reversed) when selected - a tight highlight instead
        // of a full-width bar; the path trails in dim.
        let name_style = if selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };
        let pad = 18usize.saturating_sub(w.name.chars().count()).max(1);
        spans.push(Span::styled(w.name.clone(), name_style));
        spans.push(Span::styled(
            format!("{:pad$}{path}", ""),
            Style::default().fg(Color::DarkGray),
        ));
        let lines = match worker {
            Some(worker) => vec![Line::from(spans), worker_line(worker, width)],
            None => vec![Line::from(spans)],
        };
        ListItem::new(lines)
    }

    #[cfg(test)]
    fn worker_for(&self, workspace: &Workspace) -> Option<&WorkerSnapshot> {
        self.worker_pool
            .as_ref()?
            .workers()
            .iter()
            .find(|worker| worker.workspace() == workspace.name)
    }

    /// The footer: a live prompt while entering a name or confirming a delete, a
    /// transient status message after an action, else the key hints.
    fn footer(&self) -> Paragraph<'_> {
        match &self.mode {
            Mode::NewWorkspace(buf) => Paragraph::new(Span::styled(
                format!(" new workspace: {buf}_   (enter create, esc cancel) "),
                Style::default().fg(Color::Cyan),
            )),
            Mode::ConfirmDelete(name) => Paragraph::new(Span::styled(
                format!(" delete workspace '{name}'? (y/n) "),
                Style::default().fg(Color::Red),
            )),
            Mode::ConfirmTidy => Paragraph::new(Span::styled(
                " tidy: abandon all junk empty changes? (y/n) ".to_string(),
                Style::default().fg(Color::Red),
            )),
            Mode::Pool(PoolMode::View { .. }) => Paragraph::new(Span::styled(
                " Pool: j/k select  d dispatch  s send  v review  g rebase  P PR  e alias  x dismiss  o orchestrate  a ready  r resize  D destroy  esc back ",
                Style::default().add_modifier(Modifier::DIM),
            )),
            Mode::Pool(PoolMode::Capacity { buffer, .. }) => Paragraph::new(Span::styled(
                format!(" Pool capacity: {buffer}_  (enter apply, esc cancel) "),
                Style::default().fg(Color::Cyan),
            )),
            Mode::Pool(PoolMode::TicketInput { buffer, .. }) => Paragraph::new(Span::styled(
                format!(" Ticket IDs: {buffer}_  (enter preview, esc cancel) "),
                Style::default().fg(Color::Cyan),
            )),
            Mode::Pool(PoolMode::SendInput { worker, buffer }) => Paragraph::new(Span::styled(
                format!(" Send to {worker}: {buffer}_  (enter send, esc cancel) "),
                Style::default().fg(Color::Cyan),
            )),
            Mode::Pool(PoolMode::AliasInput { worker, buffer }) => Paragraph::new(Span::styled(
                format!(" Alias for {worker}: {buffer}_  (enter save, esc cancel) "),
                Style::default().fg(Color::Cyan),
            )),
            Mode::Pool(PoolMode::DispatchPreview { tickets, .. }) => Paragraph::new(Span::styled(
                format!(
                    " Dispatch {} Ticket(s)? (enter launch, esc cancel) ",
                    tickets.len()
                ),
                Style::default().fg(Color::Cyan),
            )),
            Mode::Pool(PoolMode::ReadyPreview { .. }) => Paragraph::new(Span::styled(
                " Ready Tickets: enter launch, esc cancel ",
                Style::default().fg(Color::Cyan),
            )),
            Mode::Pool(PoolMode::ParentInput { buffer }) => Paragraph::new(Span::styled(
                format!(" Parent Ticket: {buffer}_  (enter start/resume, esc cancel) "),
                Style::default().fg(Color::Cyan),
            )),
            Mode::Pool(PoolMode::LogDetail { worker }) => Paragraph::new(Span::styled(
                format!(" Worker log {worker}: esc close "),
                Style::default().fg(Color::Cyan),
            )),
            Mode::Pool(PoolMode::ConfirmDispatchCapacity { shortage, .. }) => {
                Paragraph::new(Span::styled(
                    format!(
                        " Pool short by {} Worker(s): grow? (y/n, esc cancel) ",
                        shortage.gap()
                    ),
                    Style::default().fg(Color::Yellow),
                ))
            }
            Mode::ConfirmPoolShrink(capacity, _) => Paragraph::new(Span::styled(
                format!(" shrink Worker Pool to {capacity}? (y/n) "),
                Style::default().fg(Color::Red),
            )),
            Mode::ConfirmPoolDestroy => Paragraph::new(Span::styled(
                " destroy Worker Pool and all Worker Workspaces? (y/n) ",
                Style::default().fg(Color::Red),
            )),
            Mode::ConfirmPoolReset(worker) => Paragraph::new(Span::styled(
                format!(" reset Worker {worker} and restore its Workspace? (y/n) "),
                Style::default().fg(Color::Red),
            )),
            Mode::ConfirmPoolDismiss(worker) => Paragraph::new(Span::styled(
                format!(" dismiss Worker {worker}? (y/n) "),
                Style::default().fg(Color::Red),
            )),
            Mode::Normal => match (&self.pending_workspace, &self.status) {
                (Some(workspace), _) => Paragraph::new(Span::styled(
                    format!(" configuring '{}'... ", workspace.name),
                    Style::default().fg(Color::Yellow),
                )),
                (None, Some(msg)) => Paragraph::new(Span::styled(
                    format!(" {msg} "),
                    Style::default().fg(Color::Yellow),
                )),
                (None, None) => Paragraph::new(Span::styled(
                    if self.worker_pool.is_some() {
                        " wsg pool  ·  p manage  j/k move  ? help  q quit "
                    } else if self.world.is_some() {
                        " j/k move  J/K scroll world  ? help  q quit "
                    } else {
                        " j/k move  ? help  q quit "
                    },
                    Style::default().add_modifier(Modifier::DIM),
                )),
            },
            // Help draws its own overlay; the footer stays on the slim hint.
            Mode::Help(_) => Paragraph::new(Span::styled(
                " j/k move  ? help  q quit ",
                Style::default().add_modifier(Modifier::DIM),
            )),
            // Detail and Graph render their own full-screen footers (unreachable).
            Mode::Detail(_) | Mode::Graph(_) => Paragraph::new(Span::raw("")),
        }
    }
}

fn display_path(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

fn worker_activity_line(activity: &RunActivity) -> String {
    let detail = match activity.kind() {
        RunActivityKind::SessionStarted => "session started".to_owned(),
        RunActivityKind::Message { text } => text.clone(),
        RunActivityKind::Reasoning { text } => format!("reasoning: {text}"),
        RunActivityKind::Warning { message } => format!("warning: {message}"),
        RunActivityKind::FileChanges { paths } => format!("files: {}", paths.join(", ")),
        RunActivityKind::Plan { completed, total } => format!("plan: {completed}/{total}"),
        RunActivityKind::Tool {
            name,
            detail,
            status,
        } => format!(
            "tool {name} {status:?}{}",
            detail
                .as_deref()
                .map_or(String::new(), |value| format!(" {value}"))
        ),
        RunActivityKind::Collaboration(event) => format!("collaboration: {:?}", event),
    };
    match activity.usage() {
        Some(usage) => format!("{detail} [{}]", worker_usage_line(usage)),
        None => detail,
    }
}

fn worker_usage_line(usage: &RunUsage) -> String {
    format!(
        "input {} cached {} output {} reasoning {}",
        usage.input_tokens(),
        usage.cached_input_tokens(),
        usage.output_tokens(),
        usage.reasoning_output_tokens()
    )
}

fn worker_result_line(result: &RunResult) -> String {
    match result.conclusion() {
        RunConclusion::Succeeded => "succeeded".to_owned(),
        RunConclusion::Failed { message } => format!("failed: {message}"),
    }
}

fn worker_line(worker: &WorkerSnapshot, width: u16) -> Line<'static> {
    let status = if worker.status() == WorkerStatus::Busy && worker.has_dead_process() {
        "stale"
    } else {
        worker.status().as_str()
    };
    let ticket = worker.ticket().unwrap_or("-");
    if width < 55 {
        return Line::from(Span::styled(
            format!("  {}  {status}  ticket:{ticket}", worker.worker_id()),
            Style::default().fg(Color::Cyan),
        ));
    }
    let runtime = worker
        .agent_runtime()
        .map(|runtime| runtime.as_str())
        .unwrap_or("runtime?");
    let activity = worker
        .last_activity_at()
        .or_else(|| worker.started_at())
        .map(elapsed_label)
        .unwrap_or_else(|| "-".to_string());
    Line::from(vec![
        Span::styled("  wsg ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(
                "{}  {status:<6} {runtime:<6} ticket:{ticket:<8} {activity}",
                worker.alias()
            ),
            Style::default().fg(Color::Cyan),
        ),
    ])
}

fn elapsed_label(timestamp: &str) -> String {
    let Some(started) = parse_rfc3339_millis(timestamp) else {
        return timestamp.to_string();
    };
    let elapsed = now_millis().saturating_sub(started) / 1_000;
    if elapsed < 60 {
        format!("{elapsed}s")
    } else if elapsed < 3_600 {
        format!("{}m", elapsed / 60)
    } else if elapsed < 86_400 {
        format!("{}h", elapsed / 3_600)
    } else {
        format!("{}d", elapsed / 86_400)
    }
}

fn parse_rfc3339_millis(value: &str) -> Option<i64> {
    let (date, time) = value.strip_suffix('Z')?.split_once('T')?;
    let mut date_parts = date.split('-');
    let year = date_parts.next()?.parse::<i64>().ok()?;
    let month = date_parts.next()?.parse::<i64>().ok()?;
    let day = date_parts.next()?.parse::<i64>().ok()?;
    let mut time_parts = time.split(':');
    let hour = time_parts.next()?.parse::<i64>().ok()?;
    let minute = time_parts.next()?.parse::<i64>().ok()?;
    let seconds = time_parts.next()?;
    let (second, fraction) = seconds
        .split_once('.')
        .map_or((seconds, "0"), |parts| parts);
    let second = second.parse::<i64>().ok()?;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let fraction = fraction.chars().take(3).collect::<String>();
    let millis = if fraction.is_empty() {
        0
    } else {
        let digits = fraction.parse::<i64>().ok()?;
        digits * 10_i64.pow(3 - fraction.len() as u32)
    };
    let days = days_from_civil(year, month, day)?;
    Some(
        days.saturating_mul(86_400_000)
            .saturating_add(hour.saturating_mul(3_600_000))
            .saturating_add(minute.saturating_mul(60_000))
            .saturating_add(second.saturating_mul(1_000))
            .saturating_add(millis),
    )
}

fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

/// Width of the changed-file list pane in the detail view (borders included).
const FILES_PANE_WIDTH: u16 = 34;
/// Width of the per-workspace graph strip in the detail view (borders included).
const GRAPH_PANE_WIDTH: u16 = 34;
/// Minimum diff width to keep the strip; below this the strip is dropped so a
/// narrow terminal keeps a readable diff.
const MIN_DIFF_WIDTH: u16 = 40;
/// Minimum rows (borders included) the inline world pane needs; when half the
/// home body is shorter than this the pane is dropped so the list stays usable.
const MIN_WORLD_PANE_HEIGHT: u16 = 5;

/// Bright border when a pane has focus, dim otherwise. Shared by the diff
/// viewer's panes and the graph views.
pub(crate) fn pane_border(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// Milliseconds since the epoch, for freshness shading. Zero if the clock is
/// before the epoch (impossible in practice) - a render helper must not panic.
fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Freshness shading for a commit's change id: recently-moved commits read
/// brightest and fade to dim with age (ticket 11's optional freshness cue).
fn freshness_style(timestamp_ms: i64, now_ms: i64) -> Style {
    const HOUR: i64 = 3_600_000;
    const DAY: i64 = 24 * HOUR;
    const WEEK: i64 = 7 * DAY;
    let age = now_ms.saturating_sub(timestamp_ms);
    if age < 2 * HOUR {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else if age < WEEK {
        Style::default().fg(Color::Gray)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// A single dim connector/spacer line.
fn dim_line(s: &str) -> Line<'static> {
    Line::from(Span::styled(
        s.to_string(),
        Style::default().add_modifier(Modifier::DIM),
    ))
}

/// Truncate to `max` columns keeping the head, with a trailing ellipsis.
fn elide_right(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

/// One commit row: `<prefix><glyph><change-id>[ @]  <summary> <bookmarks>`. The
/// change id is freshness-shaded (bold when on the highlighted chain); the
/// summary is budgeted to the remaining width so a long line never wraps and
/// corrupts the layout.
#[allow(clippy::too_many_arguments)]
fn commit_line(
    prefix: &str,
    glyph: &str,
    glyph_color: Color,
    node: &graph::Node,
    is_head: bool,
    selected: bool,
    now_ms: i64,
    width: u16,
) -> Line<'static> {
    let mut id_style = freshness_style(node.timestamp_ms, now_ms);
    if selected {
        id_style = id_style.add_modifier(Modifier::BOLD);
    }
    let mut spans = vec![
        Span::raw(prefix.to_string()),
        Span::styled(glyph.to_string(), Style::default().fg(glyph_color)),
        Span::styled(node.change_id.clone(), id_style),
    ];
    if is_head {
        spans.push(Span::styled(
            " @",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let bookmarks_w: usize = node.bookmarks.iter().map(|b| b.chars().count() + 1).sum();
    let used = prefix.chars().count()
        + glyph.chars().count()
        + node.change_id.chars().count()
        + if is_head { 2 } else { 0 }
        + 1;
    let budget = (width as usize)
        .saturating_sub(used + bookmarks_w + 1)
        .max(6);
    let summary_style = if selected {
        Style::default()
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        elide_right(&node.summary, budget),
        summary_style,
    ));
    for bm in &node.bookmarks {
        spans.push(Span::styled(
            format!(" {bm}"),
            Style::default().fg(Color::Magenta),
        ));
    }
    Line::from(spans)
}

/// The world view: the commit DAG laid out like `jj log` - every mutable
/// commit, each fragment's trunk branch point, and the trunk tip, history
/// below elided as `~`. Columns and edges come from sapling-renderdag (the
/// renderer jj's own CLI uses), so the shape matches `jj log`'s; the selected
/// workspace's chain is highlighted.
fn world_graph_lines(
    g: &graph::Graph,
    selected: Option<&str>,
    now_ms: i64,
    width: u16,
) -> Vec<Line<'static>> {
    // The message slot carries a sentinel so the graph prefix can be split
    // back out of the rendered row and restyled; the text spans are ours.
    const MARK: char = '\u{1}';
    let selected_ids: HashSet<&str> = selected
        .and_then(|w| g.chain(w))
        .map(|c| {
            c.commits
                .iter()
                .map(String::as_str)
                .chain([c.head.as_str()])
                .collect()
        })
        .unwrap_or_default();

    let mut renderer = GraphRowRenderer::new()
        .output()
        .with_min_row_height(1)
        .build_box_drawing();
    let mut lines = Vec::new();
    for row in graph::log_rows(g) {
        let Some(node) = g.nodes.get(&row.id) else {
            continue;
        };
        let is_sel = selected_ids.contains(row.id.as_str());
        let glyph = if !node.wc_of.is_empty() {
            "@"
        } else if node.immutable {
            "◆"
        } else {
            "○"
        };
        let parents = row
            .edges
            .iter()
            .map(|e| match e {
                graph::LogEdge::Direct(p) => Ancestor::Parent(p.clone()),
                graph::LogEdge::Elided(p) => Ancestor::Ancestor(p.clone()),
                graph::LogEdge::Missing => Ancestor::Anonymous,
            })
            .collect();
        let rendered =
            renderer.next_row(row.id.clone(), parents, glyph.to_string(), MARK.to_string());
        for text in rendered.lines() {
            match text.split_once(MARK) {
                Some((prefix, _)) => {
                    lines.push(world_row(prefix, glyph, node, is_sel, now_ms, width));
                }
                // A pure link/termination row (fork, merge, `~`): no commit text.
                None => lines.push(dim_line(text)),
            }
        }
    }
    lines
}

/// One commit row of the world graph: the graph prefix (edges dim, the node
/// glyph coloured), then change id, `name@` working-copy badges, summary, and
/// bookmarks - sized to the pane width.
fn world_row(
    prefix: &str,
    glyph: &str,
    node: &graph::Node,
    selected: bool,
    now_ms: i64,
    width: u16,
) -> Line<'static> {
    let glyph_color = if selected {
        Color::Cyan
    } else if !node.wc_of.is_empty() {
        Color::White
    } else if node.immutable {
        Color::DarkGray
    } else {
        Color::Gray
    };
    let edge_style = Style::default().add_modifier(Modifier::DIM);
    // The glyph is the one non-edge character in the prefix; split on it so
    // the edges around it stay dim while the node itself is coloured.
    let (before, after) = prefix.split_once(glyph).unwrap_or((prefix, ""));
    let mut spans = vec![
        Span::styled(before.to_string(), edge_style),
        Span::styled(glyph.to_string(), Style::default().fg(glyph_color)),
        Span::styled(after.to_string(), edge_style),
    ];

    let mut id_style = freshness_style(node.timestamp_ms, now_ms);
    if selected {
        id_style = id_style.add_modifier(Modifier::BOLD);
    }
    spans.push(Span::styled(node.change_id.clone(), id_style));
    for ws in &node.wc_of {
        spans.push(Span::styled(
            format!(" {ws}@"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let wc_w: usize = node.wc_of.iter().map(|w| w.chars().count() + 2).sum();
    let bookmarks_w: usize = node.bookmarks.iter().map(|b| b.chars().count() + 1).sum();
    let used = prefix.chars().count() + node.change_id.chars().count() + wc_w + 1;
    let budget = (width as usize)
        .saturating_sub(used + bookmarks_w + 1)
        .max(6);
    let summary_style = if selected {
        Style::default()
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        elide_right(&node.summary, budget),
        summary_style,
    ));
    for bm in &node.bookmarks {
        spans.push(Span::styled(
            format!(" {bm}"),
            Style::default().fg(Color::Magenta),
        ));
    }
    Line::from(spans)
}

/// The per-workspace strip in the detail view: the one child past `@` (if any),
/// the workspace's own commits with connectors, and the trunk commit it attaches
/// to. Always rendered as the highlighted chain (it is the workspace in view).
fn workspace_graph_lines(
    g: &graph::Graph,
    chain: &graph::Chain,
    now_ms: i64,
    width: u16,
) -> Vec<Line<'static>> {
    let w = width.saturating_sub(2);
    let mut lines = Vec::new();

    if let Some(cid) = &chain.child
        && let Some(node) = g.nodes.get(cid)
    {
        let mut l = commit_line("", "◆ ", Color::Yellow, node, false, true, now_ms, w);
        l.spans.push(Span::styled(
            "  +1",
            Style::default().add_modifier(Modifier::DIM),
        ));
        lines.push(l);
        lines.push(dim_line("│"));
    }

    if chain.commits.is_empty() {
        lines.push(dim_line(" on trunk (clean)"));
    }
    for id in &chain.commits {
        if let Some(node) = g.nodes.get(id) {
            let is_head = id == &chain.head;
            let glyph = if is_head { "● " } else { "○ " };
            lines.push(commit_line(
                "",
                glyph,
                Color::Cyan,
                node,
                is_head,
                true,
                now_ms,
                w,
            ));
            lines.push(dim_line("│"));
        }
    }

    match chain.base.as_ref().and_then(|b| g.nodes.get(b)) {
        Some(node) => lines.push(commit_line(
            "",
            "● ",
            Color::DarkGray,
            node,
            false,
            false,
            now_ms,
            w,
        )),
        // No trunk anchor loaded: drop the trailing connector so it doesn't dangle.
        None => {
            lines.pop();
        }
    }
    lines
}

/// The per-workspace graph strip in the detail view.
fn render_graph_pane(frame: &mut Frame, graph: Option<&graph::Graph>, ws: &str, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(false))
        .title(" graph ");
    let lines: Vec<Line> = match graph {
        Some(g) => match g.chain(ws) {
            Some(chain) => workspace_graph_lines(g, chain, now_millis(), area.width),
            None => vec![dim_line(" (no chain)")],
        },
        None => vec![dim_line(" loading…")],
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// A `width` x `height` rect centered in `area`, clamped so it never exceeds the
/// frame - the popup shrinks to fit a short/narrow terminal instead of panicking.
fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    }
}

/// Colour cue for the Attention badge - the primary triage signal.
fn attention_color(att: Attention) -> Color {
    match att {
        Attention::NeedsYou => Color::Red,
        Attention::Working => Color::Green,
        Attention::ReadyToForge => Color::Cyan,
        Attention::Idle => Color::DarkGray,
    }
}

/// The agent column's label: which agent lives here (claude/codex) while a
/// session is live, `-` otherwise. The *state* is carried by the glyph and the
/// Attention grouping, so the label is free to name the agent instead.
fn agent_label(agent: agent::Agent) -> &'static str {
    match agent.state {
        AgentState::Absent | AgentState::Ended => "-",
        _ => agent.kind.label(),
    }
}

/// Colour cue for the agent cell - drawing the eye to what is live or blocked.
/// A working agent wears its brand colour, matching the animated glyph.
fn agent_color(agent: agent::Agent) -> Color {
    match agent.state {
        AgentState::Absent | AgentState::Ended => Color::DarkGray,
        AgentState::Working => brand_color(agent.kind),
        AgentState::Waiting => Color::Yellow,
        AgentState::NeedsAttention => Color::Red,
    }
}

/// The "claude is working" animation frames, straight from jj-wsx: petal glyphs
/// bounced back and forth (0 1 2 3 4 5 4 3 2 1 0 ...), one step per
/// [`Msg::Tick`].
const CLAUDE_FRAMES: [char; 6] = ['❀', '✼', '✴', '✳', '✛', '•'];

/// Codex's working animation: a hexagon filling slice by slice, then
/// restarting (nerd font `md-hexagon_slice_1..6` - needs a nerd-font-patched
/// terminal font, which the kitty setup jjfx drives already assumes).
const CODEX_FRAMES: [char; 6] = [
    '\u{f0ac3}', // 󰫃 hexagon_slice_1
    '\u{f0ac4}', // 󰫄 hexagon_slice_2
    '\u{f0ac5}', // 󰫅 hexagon_slice_3
    '\u{f0ac6}', // 󰫆 hexagon_slice_4
    '\u{f0ac7}', // 󰫇 hexagon_slice_5
    '\u{f0ac8}', // 󰫈 hexagon_slice_6
];

/// Claude's spinner orange - the colour jj-wsx gave the working animation.
const CLAUDE_ORANGE: Color = Color::Rgb(255, 149, 0);

/// Codex's cyan.
const CODEX_CYAN: Color = Color::Rgb(34, 211, 238);

/// Each agent's signature colour, worn while working. Unknown falls back to
/// claude's - the historical default agent.
fn brand_color(kind: AgentKind) -> Color {
    match kind {
        AgentKind::Codex => CODEX_CYAN,
        AgentKind::Claude | AgentKind::Unknown => CLAUDE_ORANGE,
    }
}

/// The working frame for a tick: claude's petals bloom and close (a bounce over
/// [`CLAUDE_FRAMES`]), codex's hexagon fills up and restarts (a wrap over
/// [`CODEX_FRAMES`]).
fn working_frame(kind: AgentKind, tick: u64) -> char {
    match kind {
        AgentKind::Codex => CODEX_FRAMES[(tick % CODEX_FRAMES.len() as u64) as usize],
        AgentKind::Claude | AgentKind::Unknown => {
            let len = CLAUDE_FRAMES.len() as u64;
            let cycle = (len - 1) * 2;
            let pos = tick % cycle;
            let idx = if pos < len { pos } else { cycle - pos };
            CLAUDE_FRAMES[idx as usize]
        }
    }
}

/// The one-glyph agent status ahead of the label: the agent's own working
/// animation in its brand colour, its own static mark while it waits on the
/// human (yellow; red when blocked on a permission), and a dim dot when there
/// is no live session.
fn agent_glyph(agent: agent::Agent, tick: u64) -> Span<'static> {
    let (ch, color) = match agent.state {
        AgentState::Working => (working_frame(agent.kind, tick), brand_color(agent.kind)),
        AgentState::Waiting => (paused_glyph(agent.kind), Color::Yellow),
        AgentState::NeedsAttention => (paused_glyph(agent.kind), Color::Red),
        AgentState::Absent | AgentState::Ended => ('·', Color::DarkGray),
    };
    Span::styled(format!("{ch} "), Style::default().fg(color))
}

/// The static "session present, not working" mark, per agent so a paused codex
/// cannot be mistaken for a paused claude: claude's `✻`, codex's hexagon
/// drained to its outline (nerd font `md-hexagon_outline`). The state still
/// speaks through the colour (yellow waiting, red blocked).
fn paused_glyph(kind: AgentKind) -> char {
    match kind {
        AgentKind::Codex => '\u{f02d9}', // 󰋙 hexagon_outline
        AgentKind::Claude | AgentKind::Unknown => '✻',
    }
}

/// The compact forge pipeline for a row: a `⚒` sigil then one `letter+glyph` per
/// step (`f w p r`), each coloured by its live status.
fn forge_spans(progress: &forge::Progress) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled("⚒ ", Style::default().fg(Color::Magenta))];
    for (step, status) in progress.steps() {
        spans.push(Span::styled(
            format!("{}{} ", forge_step_letter(step), forge_glyph(status)),
            Style::default().fg(forge_color(status)),
        ));
    }
    spans
}

fn forge_step_letter(step: forge::Step) -> char {
    match step {
        forge::Step::Fetch => 'f',
        forge::Step::Weld => 'w',
        forge::Step::Push => 'p',
        forge::Step::PullRequest => 'r',
    }
}

/// Glyph for a forge step's status: pending, running, done, or skipped.
fn forge_glyph(status: forge::Status) -> char {
    match status {
        forge::Status::Pending => '·',
        forge::Status::Running => '…',
        forge::Status::Ok => '✓',
        forge::Status::Skipped => '~',
    }
}

/// Colour for a forge step's status.
fn forge_color(status: forge::Status) -> Color {
    match status {
        forge::Status::Pending => Color::DarkGray,
        forge::Status::Running => Color::Cyan,
        forge::Status::Ok => Color::Green,
        forge::Status::Skipped => Color::Yellow,
    }
}

/// Colour cue for the behind-trunk count: dim when close, yellow once far enough
/// behind that `tidyws` (or a weld) is worth running.
fn behind_color(behind: u32) -> Color {
    if behind >= 5 {
        Color::Yellow
    } else {
        Color::DarkGray
    }
}

/// Colour cue for a work state - progress toward merge, plus review verdict.
fn work_color(state: WorkState) -> Color {
    use crate::work::ReviewVerdict;
    match state {
        WorkState::Unknown => Color::DarkGray,
        WorkState::Clean => Color::DarkGray,
        WorkState::Dirty { .. } => Color::Yellow,
        WorkState::Pushed => Color::Cyan,
        WorkState::PrOpen {
            verdict: ReviewVerdict::ChangesRequested,
            ..
        } => Color::Red,
        WorkState::PrOpen {
            verdict: ReviewVerdict::Approved,
            ..
        } => Color::Green,
        WorkState::PrOpen { .. } => Color::Cyan,
        WorkState::Merged => Color::Magenta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Workspace;
    use crate::workspace_dispatch::RecordingAdapter;
    use ratatui::crossterm::event::{KeyEventState, KeyModifiers};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    /// A `Terminal` that records calls instead of driving kitty, so key handling
    /// is testable without a real multiplexer. Cloning shares the recorders, so a
    /// test can keep a handle to inspect what the app asked the terminal to do.
    #[derive(Clone, Default)]
    struct FakeTerminal {
        opened: Arc<Mutex<Vec<(String, bool)>>>,
        closed: Arc<Mutex<Vec<String>>>,
        existing: Arc<Mutex<Vec<String>>>,
        open_error: Arc<Mutex<Option<String>>>,
    }

    impl Terminal for FakeTerminal {
        fn is_open(&self, name: &str) -> bool {
            self.existing.lock().unwrap().iter().any(|n| n == name)
        }
        fn open(&self, name: &str, _path: &Path, focus: bool) -> anyhow::Result<()> {
            self.opened.lock().unwrap().push((name.to_string(), focus));
            if let Some(error) = self.open_error.lock().unwrap().clone() {
                anyhow::bail!(error);
            }
            Ok(())
        }
        fn focus(&self, _name: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn close(&self, name: &str) -> anyhow::Result<()> {
            self.closed.lock().unwrap().push(name.to_string());
            Ok(())
        }
    }

    /// Programmed outcomes for [`FakeJj`]. Default is success-with-nothing-to-do
    /// (counts 0, bools false, no failure), matching an empty repo.
    #[derive(Clone, Default)]
    struct FakeOutcome {
        /// When set, every fallible mutation returns an error carrying this text.
        fail: Option<String>,
        tidyws_n: usize,
        tidy_n: usize,
        lift_ok: bool,
        lift_all_ok: bool,
    }

    /// A `Jj` that records calls instead of shelling out and returns programmed
    /// outcomes, so the destructive verbs are testable without a real repo.
    /// Cloning shares the recorders (like `FakeTerminal`); the outcome is copied.
    #[derive(Clone, Default)]
    struct FakeJj {
        lifted: Arc<Mutex<Vec<String>>>,
        lift_all_calls: Arc<Mutex<usize>>,
        tidyws_calls: Arc<Mutex<usize>>,
        tidy_calls: Arc<Mutex<usize>>,
        outcome: FakeOutcome,
    }

    impl FakeJj {
        /// `Ok(val)`, or the programmed failure when one is set.
        fn result<T>(&self, val: T) -> anyhow::Result<T> {
            match &self.outcome.fail {
                Some(e) => Err(anyhow::anyhow!("{e}")),
                None => Ok(val),
            }
        }
    }

    impl jj::Jj for FakeJj {
        fn tidyws(&self) -> anyhow::Result<usize> {
            *self.tidyws_calls.lock().unwrap() += 1;
            self.result(self.outcome.tidyws_n)
        }
        fn tidy(&self) -> anyhow::Result<usize> {
            *self.tidy_calls.lock().unwrap() += 1;
            self.result(self.outcome.tidy_n)
        }
        fn lift(&self, ws: &str) -> anyhow::Result<bool> {
            self.lifted.lock().unwrap().push(ws.to_string());
            self.result(self.outcome.lift_ok)
        }
        fn lift_all(&self) -> anyhow::Result<bool> {
            *self.lift_all_calls.lock().unwrap() += 1;
            self.result(self.outcome.lift_all_ok)
        }
    }

    fn app_with(names: &[&str]) -> App {
        app_with_terminal(names, Box::new(FakeTerminal::default()))
    }

    fn app_with_terminal(names: &[&str], terminal: Box<dyn Terminal>) -> App {
        app_full(names, terminal, Box::new(FakeJj::default()))
    }

    fn app_with_jj(names: &[&str], jj: Box<dyn jj::Jj>) -> App {
        app_full(names, Box::new(FakeTerminal::default()), jj)
    }

    fn app_full(names: &[&str], terminal: Box<dyn Terminal>, jj: Box<dyn jj::Jj>) -> App {
        let workspaces = names
            .iter()
            .map(|n| Workspace {
                name: n.to_string(),
                path: Some(PathBuf::from(format!("/wt/{n}"))),
            })
            .collect();
        // The rx end is dropped: tests fold messages by hand, they never spawn a
        // real forge, so nothing needs to receive on this channel.
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            Store::from_workspaces_for_test(PathBuf::from("/repo"), workspaces),
            agent::AgentStates::default(),
            terminal,
            jj,
            AppConfig::default(),
            false,
            tx,
        )
    }

    fn app_with_store(store: Store, terminal: Box<dyn Terminal>) -> App {
        app_with_store_and_workspace_config(store, terminal, WorkspaceConfig::default()).0
    }

    fn app_with_store_and_workspace_config(
        store: Store,
        terminal: Box<dyn Terminal>,
        workspace_config: WorkspaceConfig,
    ) -> (App, tokio::sync::mpsc::UnboundedReceiver<Msg>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let app = App::new(
            store,
            agent::AgentStates::default(),
            terminal,
            Box::new(FakeJj::default()),
            AppConfig {
                workspace: workspace_config,
                forge: ForgeConfig::default(),
            },
            false,
            tx,
        );
        (app, rx)
    }

    fn workspace_config(command: &[&str]) -> WorkspaceConfig {
        WorkspaceConfig {
            on_create: command.iter().map(|part| (*part).to_string()).collect(),
        }
    }

    fn waiting_workspace_config() -> WorkspaceConfig {
        workspace_config(&[
            "sh",
            "-c",
            "i=0; while [ \"$i\" -lt 1000 ]; do [ -f on-create-release ] && exit 0; i=$((i + 1)); sleep 0.01; done; exit 1",
        ])
    }

    fn submit_new_workspace(app: &mut App, name: &str) {
        app.handle(press(KeyCode::Char('n')));
        for character in name.chars() {
            app.handle(press(KeyCode::Char(character)));
        }
        app.handle(press(KeyCode::Enter));
    }

    async fn handle_on_create_completion(
        app: &mut App,
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<Msg>,
    ) {
        let completed = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("on-create command completes")
            .expect("completion message arrives");
        app.handle(completed);
    }

    #[test]
    fn finished_forge_with_skips_clears_the_pipeline_overlay() {
        let mut app = app_with(&["worker"]);
        let reason = "pr: no bookmark to open a PR";
        let progress = forge::Progress::finished_for_test(
            [
                forge::Status::Ok,
                forge::Status::Ok,
                forge::Status::Skipped,
                forge::Status::Skipped,
            ],
            reason,
        );

        app.handle(Msg::Forge(forge::Update::Progress {
            workspace: "worker".to_string(),
            progress: progress.clone(),
            notice: Some(reason.to_string()),
        }));
        assert!(app.forge_progress.contains_key("worker"));

        app.handle(Msg::Forge(forge::Update::Finished {
            workspace: "worker".to_string(),
            progress: Some(progress),
        }));

        assert!(!app.forge_progress.contains_key("worker"));
        assert_eq!(
            app.status.as_deref(),
            Some("worker: pr: no bookmark to open a PR")
        );
    }

    fn press(code: KeyCode) -> Msg {
        Msg::Input(Event::Key(KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }))
    }

    #[test]
    fn on_create_command_runs_exact_argv_in_workspace() {
        let dir =
            std::env::temp_dir().join(format!("jjfx-on-create-runner-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let command = vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf '%s|%s' \"$1\" \"$2\" > on-create-result".to_string(),
            "on-create".to_string(),
            "first argument".to_string(),
            "second argument".to_string(),
        ];

        run_on_create(&command, &dir).expect("command succeeds");

        assert_eq!(
            std::fs::read_to_string(dir.join("on-create-result")).unwrap(),
            "first argument|second argument"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_on_create_program_is_an_error() {
        let command = vec!["jjfx-no-such-on-create-program".to_string()];
        let error = run_on_create(&command, std::path::Path::new("/tmp"))
            .expect_err("missing program fails");

        assert!(
            error
                .to_string()
                .contains("running jjfx-no-such-on-create-program"),
            "{error}"
        );
    }

    #[test]
    fn q_and_esc_request_quit() {
        let mut app = app_with(&["default"]);
        app.handle(press(KeyCode::Char('q')));
        assert!(app.should_quit);

        let mut app = app_with(&["default"]);
        app.handle(press(KeyCode::Esc));
        assert!(app.should_quit);
    }

    #[test]
    fn pool_help_preserves_selected_worker_and_documents_pool_actions() {
        let mut app = app_with(&["default"]);
        app.mode = Mode::Pool(PoolMode::View {
            selected: Some("worker-01".to_owned()),
        });

        app.handle(press(KeyCode::Char('?')));
        assert!(matches!(app.mode, Mode::Help(_)));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Send to Worker"), "{text}");
        assert!(text.contains("Reset Worker"), "{text}");

        app.handle(press(KeyCode::Esc));
        assert!(matches!(
            app.mode,
            Mode::Pool(PoolMode::View { ref selected }) if selected.as_deref() == Some("worker-01")
        ));
    }

    #[test]
    fn help_toggles_without_touching_state() {
        let mut app = app_with(&["default", "feat"]);
        // Move selection off the top so we can prove Help leaves it untouched.
        app.handle(press(KeyCode::Down));
        let before = app.list.selected().map(str::to_string);

        app.handle(press(KeyCode::Char('?')));
        assert!(matches!(app.mode, Mode::Help(_)));
        // Navigation is swallowed while the overlay is open, and no status leaks.
        app.handle(press(KeyCode::Down));
        assert!(matches!(app.mode, Mode::Help(_)));
        assert_eq!(app.list.selected().map(str::to_string), before);
        assert!(app.status.is_none());

        // Esc closes it; reopening and pressing ? again also closes.
        app.handle(press(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Normal));
        app.handle(press(KeyCode::Char('?')));
        app.handle(press(KeyCode::Char('?')));
        assert!(matches!(app.mode, Mode::Normal));
        assert!(!app.should_quit);
    }

    #[test]
    fn help_overlay_renders_a_bordered_box() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = app_with(&["default", "feat"]);
        app.mode = Mode::Help(Box::new(Mode::Normal));
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| app.render(f)).unwrap();

        // The title and a binding row must be present in the drawn buffer.
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Keybindings"));
        assert!(text.contains("Open workspace"));
    }

    #[test]
    fn help_overlay_clamps_to_a_tiny_terminal() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        // A terminal smaller than the popup must clamp, not panic.
        let mut app = app_with(&["default"]);
        app.mode = Mode::Help(Box::new(Mode::Normal));
        let mut term = Terminal::new(TestBackend::new(6, 3)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
    }

    #[test]
    fn bindings_cover_the_essential_keys() {
        let normal_keys: Vec<&str> = NORMAL_BINDINGS.iter().map(|(_, k)| *k).collect();
        let pool_keys: Vec<&str> = POOL_BINDINGS.iter().map(|(_, k)| *k).collect();
        assert!(normal_keys.contains(&"j / ↓"));
        assert!(pool_keys.contains(&"s"));
        assert!(pool_keys.contains(&"K"));
        assert!(normal_keys.contains(&"?"));
        assert!(pool_keys.contains(&"?"));
        assert!(normal_keys.contains(&"q / esc"));
        assert!(
            NORMAL_BINDINGS
                .iter()
                .chain(POOL_BINDINGS)
                .all(|(label, key)| !label.is_empty() && !key.is_empty())
        );
    }

    #[test]
    fn n_enters_name_mode_and_filters_to_safe_chars() {
        let mut app = app_with(&["default"]);
        app.handle(press(KeyCode::Char('n')));
        // Only alphanumerics, '-', '_' are accepted into the buffer.
        for c in ['f', 'e', 'a', 't', '/', ' ', '-', '1'] {
            app.handle(press(KeyCode::Char(c)));
        }
        match &app.mode {
            Mode::NewWorkspace(buf) => assert_eq!(buf, "feat-1"),
            _ => panic!("expected NewWorkspace mode"),
        }
        // Esc cancels back to Normal without creating anything.
        app.handle(press(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn d_on_default_is_refused_without_confirmation() {
        let mut app = app_with(&["default", "feat"]);
        // Selection starts on default (index 0).
        app.handle(press(KeyCode::Char('d')));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(
            app.status.as_deref(),
            Some("the default workspace cannot be deleted")
        );
        // On a non-default workspace, d asks for confirmation.
        app.handle(press(KeyCode::Down));
        app.handle(press(KeyCode::Char('d')));
        assert!(matches!(app.mode, Mode::ConfirmDelete(ref n) if n == "feat"));
        // n cancels.
        app.handle(press(KeyCode::Char('n')));
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn configuring_workspace_blocks_mutations_that_could_touch_it() {
        let fake_jj = FakeJj::default();
        let fake_terminal = FakeTerminal::default();
        let mut app = app_full(
            &["default", "feat"],
            Box::new(fake_terminal.clone()),
            Box::new(fake_jj.clone()),
        );
        app.pending_workspace = Some(PendingWorkspace {
            name: "feat".to_string(),
            path: PathBuf::from("/wt/feat"),
        });
        app.handle(press(KeyCode::Down));

        for key in [
            KeyCode::Char('d'),
            KeyCode::Char('r'),
            KeyCode::Char('R'),
            KeyCode::Char('t'),
            KeyCode::Char('T'),
            KeyCode::Char('f'),
            KeyCode::Char('F'),
        ] {
            app.handle(press(key));
            assert!(matches!(app.mode, Mode::Normal));
            assert_eq!(
                app.status.as_deref(),
                Some("workspace 'feat' is still configuring")
            );
        }

        assert!(fake_terminal.closed.lock().unwrap().is_empty());
        assert!(fake_jj.lifted.lock().unwrap().is_empty());
        assert_eq!(*fake_jj.lift_all_calls.lock().unwrap(), 0);
        assert_eq!(*fake_jj.tidyws_calls.lock().unwrap(), 0);
        assert_eq!(*fake_jj.tidy_calls.lock().unwrap(), 0);
    }

    #[test]
    fn enter_focuses_existing_tab_and_o_opens_background() {
        let fake = FakeTerminal::default();
        // Pretend "default"'s tab already exists.
        fake.existing.lock().unwrap().push("default".to_string());
        let mut app = app_with_terminal(&["default", "feat"], Box::new(fake.clone()));

        // enter on an existing tab -> focus, no new open.
        app.handle(press(KeyCode::Enter));
        assert!(fake.opened.lock().unwrap().is_empty());

        // o on a not-yet-open workspace -> background open (focus=false).
        app.handle(press(KeyCode::Down));
        app.handle(press(KeyCode::Char('o')));
        assert_eq!(
            fake.opened.lock().unwrap().as_slice(),
            &[("feat".to_string(), false)]
        );
    }

    #[test]
    fn a_configuring_workspace_cannot_be_opened() {
        let fake = FakeTerminal::default();
        let mut app = app_with_terminal(&["default", "feat"], Box::new(fake.clone()));
        app.pending_workspace = Some(PendingWorkspace {
            name: "feat".to_string(),
            path: PathBuf::from("/wt/feat"),
        });

        app.handle(press(KeyCode::Down));
        app.handle(press(KeyCode::Enter));

        assert!(fake.opened.lock().unwrap().is_empty());
        assert_eq!(
            app.status.as_deref(),
            Some("workspace 'feat' is still configuring")
        );
    }

    #[test]
    fn configuring_footer_remains_visible_without_a_status_message() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = app_with(&["default", "feat"]);
        app.pending_workspace = Some(PendingWorkspace {
            name: "feat".to_string(),
            path: PathBuf::from("/wt/feat"),
        });
        app.status = None;
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();

        terminal.draw(|frame| app.render(frame)).unwrap();

        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("configuring 'feat'..."), "{text}");
    }

    #[test]
    fn completed_on_create_does_not_open_a_missing_workspace_path() {
        let fake = FakeTerminal::default();
        let mut app = app_with_terminal(&["default", "feat"], Box::new(fake.clone()));
        let path = PathBuf::from("/jjfx-test-path-that-does-not-exist/feat");
        app.store.workspace("feat").unwrap();
        app.pending_workspace = Some(PendingWorkspace {
            name: "feat".to_string(),
            path: path.clone(),
        });

        app.handle(Msg::WorkspaceConfigured {
            workspace: PendingWorkspace {
                name: "feat".to_string(),
                path,
            },
            result: Ok(()),
        });

        assert!(fake.opened.lock().unwrap().is_empty());
        assert!(app.pending_workspace.is_none());
        assert_eq!(
            app.status.as_deref(),
            Some("created 'feat', on-create finished after the workspace disappeared")
        );
    }

    #[test]
    fn agent_event_updates_the_matching_workspace_row() {
        let mut app = app_with(&["default", "feat"]);
        // canon() no-ops on nonexistent paths, so /wt/feat matches the row path.
        app.handle(Msg::AgentEvent(agent::Event {
            name: "UserPromptSubmit".into(),
            cwd: "/wt/feat".into(),
            transcript_path: None,
        }));
        let feat = app
            .store
            .workspaces()
            .iter()
            .find(|w| w.name == "feat")
            .unwrap();
        assert_eq!(app.agent_state(feat), AgentState::Working);
        // A workspace with no events stays Absent.
        let def = app
            .store
            .workspaces()
            .iter()
            .find(|w| w.name == "default")
            .unwrap();
        assert_eq!(app.agent_state(def), AgentState::Absent);
    }

    #[test]
    fn work_snapshot_updates_the_matching_row_by_name() {
        use crate::work::Work;
        let mut app = app_with(&["default", "feat"]);
        let mut snap = HashMap::new();
        snap.insert(
            "feat".to_string(),
            Work {
                state: WorkState::Dirty {
                    added: 9,
                    removed: 2,
                },
                behind: 4,
            },
        );
        app.handle(Msg::WorkSnapshot(snap));
        let feat = app
            .store
            .workspaces()
            .iter()
            .find(|w| w.name == "feat")
            .unwrap();
        assert_eq!(
            app.work_state(feat),
            WorkState::Dirty {
                added: 9,
                removed: 2
            }
        );
        assert_eq!(app.behind(feat), 4);
        // A workspace absent from the snapshot stays Unknown with zero behind.
        let def = app
            .store
            .workspaces()
            .iter()
            .find(|w| w.name == "default")
            .unwrap();
        assert_eq!(app.work_state(def), WorkState::Unknown);
        assert_eq!(app.behind(def), 0);
    }

    #[test]
    fn ticket_input_preserves_order_until_dispatch_preview() {
        let temp = tempfile::tempdir().unwrap();
        let pool = temp.path().join(".jj/pool");
        std::fs::create_dir_all(&pool).unwrap();
        std::fs::write(
            temp.path().join(".jj/pool.json"),
            br#"{"size":1,"gh_repo":"Jarvvski/jjfx","workers":["worker-01"],"created_at":"2026-07-27T10:00:00Z","names":{"worker-01":"alpha"}}"#,
        )
        .unwrap();
        std::fs::write(
            pool.join("worker-01.json"),
            br#"{"status":"idle","agent":"claude","ticket":null,"pid":null,"started_at":null,"completed_at":null,"log_file":null,"branch_name":null,"exit_code":null,"error":null}"#,
        )
        .unwrap();
        let snapshot = wsg_core::Repository::open(temp.path())
            .unwrap()
            .read_worker_pool_snapshot();
        let mut app = app_with(&["default", "worker-01"]);
        app.worker_pool = Some(snapshot);
        app.handle(press(KeyCode::Char('p')));
        app.handle(press(KeyCode::Char('d')));
        for character in "ENG-42 ENG-43".chars() {
            app.handle(press(KeyCode::Char(character)));
        }
        app.handle(press(KeyCode::Enter));

        assert!(matches!(
            app.mode,
            Mode::Pool(PoolMode::DispatchPreview { ref tickets, .. })
                if tickets == &["ENG-42".to_owned(), "ENG-43".to_owned()]
        ));
        app.handle(press(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Pool(PoolMode::View { .. })));
        assert_eq!(app.active_operation, None);
    }

    #[test]
    fn ready_ticket_event_opens_an_ordered_preview() {
        let mut app = app_with(&["default"]);
        app.mode = Mode::Pool(PoolMode::View { selected: None });
        app.active_operation = Some(4);
        app.handle(Msg::WorkspaceDispatch(
            WorkspaceDispatchEvent::ReadyTickets {
                operation: 4,
                result: crate::workspace_dispatch::ReadyTicketResult::new(
                    vec![
                        crate::workspace_dispatch::ReadyTicket::new("ENG-42", "First"),
                        crate::workspace_dispatch::ReadyTicket::new("ENG-43", "Second"),
                    ],
                    vec!["ENG-44: invalid status".to_owned()],
                ),
            },
        ));

        assert!(matches!(
            app.mode,
            Mode::Pool(PoolMode::ReadyPreview { .. })
        ));
        assert_eq!(app.active_operation, None);
        assert_eq!(
            app.ready_tickets.as_ref().unwrap().tickets()[1].id(),
            "ENG-43"
        );
    }

    #[test]
    fn capacity_shortage_requires_a_typed_growth_decision() {
        let mut app = app_with(&["default"]);
        app.mode = Mode::Pool(PoolMode::View { selected: None });
        app.active_operation = Some(7);
        app.handle(Msg::WorkspaceDispatch(
            WorkspaceDispatchEvent::DispatchCapacity {
                operation: 7,
                tickets: vec!["ENG-42".to_owned(), "ENG-43".to_owned()],
                worker: None,
                shortage: crate::workspace_dispatch::DispatchCapacityShortage::new(2, 1),
            },
        ));

        assert!(matches!(
            app.mode,
            Mode::Pool(PoolMode::ConfirmDispatchCapacity { ref tickets, shortage, .. })
                if tickets == &["ENG-42".to_owned(), "ENG-43".to_owned()]
                    && shortage.gap() == 1
        ));
        app.handle(press(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Pool(PoolMode::View { .. })));
        assert_eq!(app.active_operation, None);
        assert_eq!(app.status.as_deref(), Some("Dispatch cancelled"));
    }

    #[test]
    fn dispatch_input_remains_responsive_while_background_operation_is_active() {
        let mut app = app_with(&["default"]);
        app.mode = Mode::Pool(PoolMode::TicketInput {
            buffer: String::new(),
            selected: None,
        });
        app.active_operation = Some(8);
        app.handle(press(KeyCode::Char('E')));
        app.handle(press(KeyCode::Char('N')));
        app.handle(press(KeyCode::Char('G')));

        assert!(matches!(
            app.mode,
            Mode::Pool(PoolMode::TicketInput { ref buffer, .. }) if buffer == "ENG"
        ));
        assert_eq!(app.active_operation, Some(8));
    }

    #[test]
    fn pool_send_editor_submits_a_prompt_without_blocking_input() {
        let temp = tempfile::tempdir().unwrap();
        let pool = temp.path().join(".jj/pool");
        std::fs::create_dir_all(&pool).unwrap();
        std::fs::write(
            temp.path().join(".jj/pool.json"),
            br#"{"size":1,"gh_repo":"Jarvvski/jjfx","workers":["worker-01"],"created_at":"2026-07-27T10:00:00Z","names":{"worker-01":"alpha"},"agent":"claude"}"#,
        )
        .unwrap();
        std::fs::write(
            pool.join("worker-01.json"),
            br#"{"status":"idle","agent":"claude","ticket":null,"pid":null,"started_at":null,"completed_at":null,"log_file":null,"branch_name":null,"exit_code":null,"error":null}"#,
        )
        .unwrap();
        let snapshot = wsg_core::Repository::open(temp.path())
            .unwrap()
            .read_worker_pool_snapshot();
        let adapter = RecordingAdapter::new(snapshot.clone());
        let controller = WorkspaceDispatchController::new(adapter.clone(), |_| {});
        let mut app = app_with(&["default", "worker-01"]);
        app.worker_pool = Some(snapshot);
        app.dispatch = controller;
        app.mode = Mode::Pool(PoolMode::View {
            selected: Some("worker-01".to_owned()),
        });

        app.handle(press(KeyCode::Char('s')));
        assert!(matches!(
            app.mode,
            Mode::Pool(PoolMode::SendInput { ref worker, ref buffer })
                if worker == "worker-01" && buffer.is_empty()
        ));
        for character in "continue the work".chars() {
            app.handle(press(KeyCode::Char(character)));
        }
        app.handle(press(KeyCode::Enter));

        for _ in 0..100 {
            if adapter.commands().iter().any(|command| {
                matches!(
                    command,
                    WorkspaceDispatchCommand::Send { worker, prompt, .. }
                        if worker == "worker-01" && prompt == "continue the work"
                )
            }) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(adapter.commands().iter().any(|command| {
            matches!(
                command,
                WorkspaceDispatchCommand::Send { worker, prompt, .. }
                    if worker == "worker-01" && prompt == "continue the work"
            )
        }));
        assert!(matches!(app.mode, Mode::Pool(PoolMode::View { .. })));
        assert!(app.active_operation.is_some());
    }

    #[test]
    fn worker_action_event_keeps_session_outcome_visible() {
        let temp = tempfile::tempdir().unwrap();
        let output = std::process::Command::new("jj")
            .args(["--config", "signing.behavior=drop", "git", "init"])
            .arg(temp.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        let mut app = app_with(&["default"]);
        app.worker_pool = Some(
            wsg_core::Repository::open(temp.path())
                .unwrap()
                .read_worker_pool_snapshot(),
        );
        app.mode = Mode::Pool(PoolMode::View { selected: None });
        app.active_operation = Some(12);
        app.handle(Msg::WorkspaceDispatch(
            WorkspaceDispatchEvent::WorkerActionCompleted {
                operation: 12,
                outcome: WorkerSessionOutcome::new(
                    "worker-01",
                    crate::workspace_dispatch::WorkerActionKind::Send,
                    wsg_core::AgentRuntime::Claude,
                    AgentSessionResolution::Fresh {
                        reason: wsg_core::FreshSessionReason::MissingIdentity,
                    },
                    77,
                ),
            },
        ));

        assert!(app.active_operation.is_none());
        assert!(
            app.status
                .as_deref()
                .is_some_and(|status| status.contains("Send worker-01"))
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 12)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            text.contains("fresh session (log has no session id yet)"),
            "{text}"
        );
    }

    #[test]
    fn pool_worker_actions_use_keys_confirmation_and_visible_results() {
        let temp = tempfile::tempdir().unwrap();
        let pool = temp.path().join(".jj/pool");
        std::fs::create_dir_all(&pool).unwrap();
        std::fs::write(
            temp.path().join(".jj/pool.json"),
            br#"{"size":1,"gh_repo":"Jarvvski/jjfx","workers":["worker-01"],"created_at":"2026-07-27T10:00:00Z","names":{"worker-01":"alpha"},"agent":"claude"}"#,
        )
        .unwrap();
        std::fs::write(
            pool.join("worker-01.json"),
            br#"{"status":"idle","agent":"claude","ticket":null,"pid":null,"started_at":null,"completed_at":null,"log_file":null,"branch_name":null,"exit_code":null,"error":null}"#,
        )
        .unwrap();
        let snapshot = wsg_core::Repository::open(temp.path())
            .unwrap()
            .read_worker_pool_snapshot();
        let adapter = RecordingAdapter::new(snapshot.clone());
        let controller = WorkspaceDispatchController::new(adapter.clone(), |_| {});
        let mut app = app_with(&["default", "worker-01"]);
        app.worker_pool = Some(snapshot);
        app.dispatch = controller;
        app.mode = Mode::Pool(PoolMode::View {
            selected: Some("worker-01".to_owned()),
        });

        app.handle(press(KeyCode::Char('e')));
        assert!(
            matches!(app.mode, Mode::Pool(PoolMode::AliasInput { ref worker, .. }) if worker == "worker-01")
        );
        app.handle(press(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Pool(PoolMode::View { .. })));
        assert!(
            !adapter
                .commands()
                .iter()
                .any(|command| matches!(command, WorkspaceDispatchCommand::SetAlias { .. }))
        );
        app.handle(press(KeyCode::Char('e')));
        for _ in 0..5 {
            app.handle(press(KeyCode::Backspace));
        }
        for character in "primary".chars() {
            app.handle(press(KeyCode::Char(character)));
        }
        app.handle(press(KeyCode::Enter));
        app.handle(Msg::WorkspaceDispatch(
            WorkspaceDispatchEvent::WorkerCommandCompleted {
                operation: app.active_operation.unwrap(),
                result: WorkerCommandResult::AliasChanged {
                    worker: "worker-01".to_owned(),
                    alias: Some("primary".to_owned()),
                },
            },
        ));
        assert!(
            app.status
                .as_deref()
                .is_some_and(|status| status.contains("primary"))
        );

        app.mode = Mode::Pool(PoolMode::View {
            selected: Some("worker-01".to_owned()),
        });
        app.handle(press(KeyCode::Char('x')));
        assert!(matches!(app.mode, Mode::ConfirmPoolDismiss(ref worker) if worker == "worker-01"));
        app.handle(press(KeyCode::Char('n')));
        assert!(matches!(app.mode, Mode::Pool(PoolMode::View { .. })));
        app.handle(press(KeyCode::Char('x')));
        app.handle(press(KeyCode::Char('y')));
        for _ in 0..100 {
            if adapter.commands().iter().any(|command| {
                matches!(command, WorkspaceDispatchCommand::Dismiss { worker, .. } if worker == "worker-01")
            }) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(adapter.commands().iter().any(|command| matches!(
            command,
            WorkspaceDispatchCommand::Dismiss { worker, .. } if worker == "worker-01"
        )));

        app.mode = Mode::Pool(PoolMode::View {
            selected: Some("worker-01".to_owned()),
        });
        app.handle(press(KeyCode::Char('g')));
        app.handle(press(KeyCode::Char('P')));
        for _ in 0..100 {
            if adapter.commands().iter().any(|command| {
                matches!(command, WorkspaceDispatchCommand::OpenPullRequest { worker, .. } if worker == "worker-01")
            }) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(adapter.commands().iter().any(|command| matches!(
            command,
            WorkspaceDispatchCommand::Rebase { worker, .. } if worker == "worker-01"
        )));
        assert!(adapter.commands().iter().any(|command| matches!(
            command,
            WorkspaceDispatchCommand::OpenPullRequest { worker, .. } if worker == "worker-01"
        )));
        let operation = app.active_operation.expect("Pull Request operation");
        app.handle(Msg::WorkspaceDispatch(WorkspaceDispatchEvent::Failed {
            operation,
            message: "browser unavailable".to_owned(),
        }));
        assert_eq!(app.active_operation, None);
        assert_eq!(
            app.status.as_deref(),
            Some("Worker Pool: browser unavailable")
        );
    }

    #[test]
    fn pool_reset_requires_confirmation_and_keeps_restoration_observable() {
        let mut app = app_with(&["default"]);
        app.mode = Mode::Pool(PoolMode::View {
            selected: Some("worker-01".to_owned()),
        });
        app.handle(press(KeyCode::Char('K')));
        assert!(matches!(
            app.mode,
            Mode::ConfirmPoolReset(ref worker) if worker == "worker-01"
        ));
        app.handle(press(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Pool(PoolMode::View { .. })));

        app.mode = Mode::Pool(PoolMode::View {
            selected: Some("worker-01".to_owned()),
        });
        app.active_operation = Some(8);
        app.worker_reset_operation = Some(8);
        app.handle(Msg::WorkspaceDispatch(
            WorkspaceDispatchEvent::WorkerResetCompleted {
                operation: 8,
                outcome: crate::workspace_dispatch::WorkerResetOutcome::new(
                    "worker-01",
                    wsg_core::RunReset::Abandoned {
                        terminated_pid: Some(77),
                    },
                ),
            },
        ));
        assert_eq!(app.active_operation, Some(8));
        assert!(
            app.status
                .as_deref()
                .is_some_and(|status| status.contains("Abandoned"))
        );
        app.handle(Msg::WorkspaceDispatch(
            WorkspaceDispatchEvent::WorkspaceRestorationCompleted {
                operation: 8,
                worker: "worker-01".to_owned(),
                result: crate::workspace_dispatch::WorkspaceRestorationResult::Restored,
            },
        ));
        assert_eq!(app.active_operation, None);
        assert_eq!(app.worker_reset_operation, None);
        assert!(
            app.status
                .as_deref()
                .is_some_and(|status| status.contains("Workspace restored"))
        );
    }

    #[test]
    fn parent_orchestration_uses_o_without_changing_direct_dispatch_d() {
        let temp = tempfile::tempdir().unwrap();
        let output = std::process::Command::new("jj")
            .args(["--config", "signing.behavior=drop", "git", "init"])
            .arg(temp.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        let snapshot = wsg_core::Repository::open(temp.path())
            .unwrap()
            .read_worker_pool_snapshot();
        let adapter = RecordingAdapter::new(snapshot);
        let controller = WorkspaceDispatchController::new(adapter.clone(), |_| {});
        let mut app = app_with(&["default"]);
        app.dispatch = controller;
        app.mode = Mode::Pool(PoolMode::View { selected: None });

        app.handle(press(KeyCode::Char('o')));
        for character in "ENG-100".chars() {
            app.handle(press(KeyCode::Char(character)));
        }
        assert!(matches!(
            app.mode,
            Mode::Pool(PoolMode::ParentInput { ref buffer }) if buffer == "ENG-100"
        ));
        app.handle(press(KeyCode::Enter));
        assert!(matches!(app.mode, Mode::Pool(PoolMode::View { .. })));
        for _ in 0..100 {
            if adapter.commands().iter().any(|command| {
                matches!(
                    command,
                    WorkspaceDispatchCommand::Orchestrate { parent, .. }
                        if parent == "ENG-100"
                )
            }) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(adapter.commands().iter().any(|command| {
            matches!(
                command,
                WorkspaceDispatchCommand::Orchestrate { parent, .. }
                    if parent == "ENG-100"
            )
        }));
        assert!(app.orchestration_operation.is_some());
    }

    #[test]
    fn worker_log_detail_renders_provider_neutral_codex_activity_and_result() {
        let temp = tempfile::tempdir().unwrap();
        let output = std::process::Command::new("jj")
            .args(["--config", "signing.behavior=drop", "git", "init"])
            .arg(temp.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        let mut app = app_with(&["default"]);
        app.worker_pool = Some(
            wsg_core::Repository::open(temp.path())
                .unwrap()
                .read_worker_pool_snapshot(),
        );
        app.worker_log = Some(crate::workspace_dispatch::WorkerLogSnapshot::new(
            "worker-01",
            wsg_core::AgentRuntime::Codex,
            Some(RunActivity::new(RunActivityKind::FileChanges {
                paths: vec![
                    "src/app.rs".to_owned(),
                    "src/workspace_dispatch.rs".to_owned(),
                ],
            })),
            Some(RunResult::failed("provider stopped")),
        ));
        app.mode = Mode::Pool(PoolMode::LogDetail {
            worker: "worker-01".to_owned(),
        });

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 12)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("runtime: codex"), "{text}");
        assert!(
            text.contains("files: src/app.rs, src/workspace_dispatch.rs"),
            "{text}"
        );
        assert!(text.contains("result: failed: provider stopped"), "{text}");
    }

    #[test]
    fn stale_worker_log_events_do_not_replace_the_current_detail() {
        let mut app = app_with(&["default"]);
        app.mode = Mode::Pool(PoolMode::LogDetail {
            worker: "worker-01".to_owned(),
        });
        app.worker_log_operation = Some(7);
        let first = crate::workspace_dispatch::WorkerLogSnapshot::new(
            "worker-01",
            wsg_core::AgentRuntime::Claude,
            Some(RunActivity::new(RunActivityKind::Message {
                text: "current".to_owned(),
            })),
            None,
        );
        app.worker_log = Some(first.clone());
        let stale = crate::workspace_dispatch::WorkerLogSnapshot::new(
            "worker-01",
            wsg_core::AgentRuntime::Codex,
            Some(RunActivity::new(RunActivityKind::Message {
                text: "stale".to_owned(),
            })),
            None,
        );
        app.handle(Msg::WorkspaceDispatch(
            WorkspaceDispatchEvent::WorkerLogUpdated {
                operation: 6,
                snapshot: Box::new(stale),
            },
        ));
        assert_eq!(app.worker_log.as_ref(), Some(&first));
        assert_eq!(app.worker_log_operation, Some(7));

        let terminal = crate::workspace_dispatch::WorkerLogSnapshot::new(
            "worker-01",
            wsg_core::AgentRuntime::Claude,
            None,
            Some(RunResult::succeeded()),
        );
        app.handle(Msg::WorkspaceDispatch(
            WorkspaceDispatchEvent::WorkerLogUpdated {
                operation: 7,
                snapshot: Box::new(terminal),
            },
        ));
        assert_eq!(app.worker_log_operation, None);
        assert_eq!(
            app.worker_log
                .as_ref()
                .unwrap()
                .result()
                .unwrap()
                .conclusion(),
            &RunConclusion::Succeeded
        );
    }

    #[test]
    fn unavailable_worker_log_remains_understandable_on_a_narrow_terminal() {
        let mut app = app_with(&["default"]);
        app.mode = Mode::Pool(PoolMode::LogDetail {
            worker: "worker-01".to_owned(),
        });
        app.worker_log_error = Some("log file unavailable: missing Worker log".to_owned());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(20, 5)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
    }

    #[test]
    fn ordered_dispatch_outcomes_render_with_ticket_titles() {
        let mut app = app_with(&["default"]);
        let temp = tempfile::tempdir().unwrap();
        let output = std::process::Command::new("jj")
            .args(["--config", "signing.behavior=drop", "git", "init"])
            .arg(temp.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        app.worker_pool = Some(
            wsg_core::Repository::open(temp.path())
                .unwrap()
                .read_worker_pool_snapshot(),
        );
        app.dispatch_result = Some(crate::workspace_dispatch::DispatchResult::new(
            vec![
                crate::workspace_dispatch::DispatchOutcome::success(
                    "ENG-42".to_owned(),
                    "First Ticket".to_owned(),
                    "worker-01".to_owned(),
                    42,
                ),
                crate::workspace_dispatch::DispatchOutcome::failure(
                    "ENG-43".to_owned(),
                    "Second Ticket".to_owned(),
                    None,
                    wsg_core::DirectDispatchFailurePhase::Capacity,
                    "no idle Worker".to_owned(),
                ),
            ],
            true,
        ));
        app.mode = Mode::Pool(PoolMode::View { selected: None });
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 12)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("ENG-42  First Ticket"), "{text}");
        assert!(text.contains("ENG-43 [Capacity]"), "{text}");
        assert!(text.find("ENG-42").unwrap() < text.find("ENG-43").unwrap());
    }

    #[test]
    fn dispatch_group_progress_renders_ready_wave_and_terminal_counts() {
        use std::collections::BTreeMap;

        let temp = tempfile::tempdir().unwrap();
        let output = std::process::Command::new("jj")
            .args(["--config", "signing.behavior=drop", "git", "init"])
            .arg(temp.path())
            .output()
            .unwrap();
        assert!(output.status.success());

        let parent = wsg_core::TicketId::parse("ENG-100").unwrap();
        let blocker = wsg_core::TicketId::parse("ENG-101").unwrap();
        let dependent = wsg_core::TicketId::parse("ENG-102").unwrap();
        let mut sub_issues = BTreeMap::new();
        sub_issues.insert(
            blocker,
            wsg_core::SubIssueState::new(
                "Foundation",
                wsg_core::WireStatus::new("done"),
                Vec::new(),
            ),
        );
        sub_issues.insert(
            dependent,
            wsg_core::SubIssueState::new(
                "Dependent work",
                wsg_core::WireStatus::new("pending"),
                vec![wsg_core::TicketId::parse("ENG-101").unwrap()],
            ),
        );
        let mut state = wsg_core::DispatchGroupState::new(
            parent,
            wsg_core::WireTimestamp::new("2026-08-10T10:00:00Z"),
            "Jarvvski/jjfx",
            wsg_core::DispatchGroupOptions::new(""),
        );
        state.sub_issues = sub_issues;

        let mut app = app_with(&["default"]);
        app.worker_pool = Some(
            wsg_core::Repository::open(temp.path())
                .unwrap()
                .read_worker_pool_snapshot(),
        );
        let progress = crate::workspace_dispatch::DispatchGroupProgress::from_state(state).unwrap();
        app.mode = Mode::Pool(PoolMode::View { selected: None });
        app.handle(Msg::WorkspaceDispatch(
            WorkspaceDispatchEvent::GroupProgress {
                operation: 0,
                progress,
            },
        ));

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 15)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Dispatch Group ENG-100"), "{text}");
        assert!(text.contains("ready: ENG-102"), "{text}");
        assert!(text.contains("wave 2"), "{text}");
        assert!(text.contains("done: 1"), "{text}");
    }

    #[test]
    fn pool_capacity_input_survives_refresh_and_cancel_submits_no_resize() {
        let temp = tempfile::tempdir().unwrap();
        let pool = temp.path().join(".jj/pool");
        std::fs::create_dir_all(&pool).unwrap();
        std::fs::write(
            temp.path().join(".jj/pool.json"),
            br#"{"size":1,"gh_repo":"Jarvvski/jjfx","workers":["worker-01"],"created_at":"2026-07-27T10:00:00Z","names":{"worker-01":"alpha"}}"#,
        )
        .unwrap();
        std::fs::write(
            pool.join("worker-01.json"),
            br#"{"status":"idle","agent":"claude","ticket":null,"pid":null,"started_at":null,"completed_at":null,"log_file":null,"branch_name":null,"exit_code":null,"error":null}"#,
        )
        .unwrap();
        let snapshot = wsg_core::Repository::open(temp.path())
            .unwrap()
            .read_worker_pool_snapshot();
        let adapter = RecordingAdapter::new(snapshot.clone());
        let controller = WorkspaceDispatchController::new(adapter.clone(), |_| {});
        let mut app = app_with(&["default", "worker-01"]);
        app.dispatch = controller;
        app.handle(Msg::WorkspaceDispatch(WorkspaceDispatchEvent::Snapshot {
            operation: 0,
            snapshot: snapshot.clone(),
        }));
        app.handle(press(KeyCode::Char('p')));
        app.handle(press(KeyCode::Char('r')));
        app.handle(press(KeyCode::Backspace));
        app.handle(press(KeyCode::Char('2')));
        app.handle(Msg::WorkspaceDispatch(WorkspaceDispatchEvent::Snapshot {
            operation: 0,
            snapshot,
        }));

        assert!(matches!(
            app.mode,
            Mode::Pool(PoolMode::Capacity { ref buffer, .. }) if buffer == "2"
        ));
        app.handle(press(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Pool(PoolMode::View { .. })));
        assert!(
            !adapter
                .commands()
                .iter()
                .any(|command| matches!(command, WorkspaceDispatchCommand::Resize { .. }))
        );
        app.active_operation = Some(3);
        app.handle(Msg::WorkspaceDispatch(WorkspaceDispatchEvent::Failed {
            operation: 3,
            message: "refresh failed".to_string(),
        }));
        assert!(app.worker_pool.is_some());
        assert_eq!(app.status.as_deref(), Some("Worker Pool: refresh failed"));
    }

    #[test]
    fn pool_mode_renders_on_a_narrow_terminal() {
        let mut app = app_with(&["default"]);
        app.handle(press(KeyCode::Char('p')));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(20, 4)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Worker Pool"), "{text}");
    }

    #[test]
    fn stale_workspace_dispatch_events_do_not_replace_newer_operations() {
        let mut app = app_with(&["default"]);
        app.active_operation = Some(9);
        app.handle(Msg::WorkspaceDispatch(WorkspaceDispatchEvent::Failed {
            operation: 8,
            message: "old failure".to_string(),
        }));
        assert_eq!(app.active_operation, Some(9));
        assert_eq!(app.status, None);
    }

    #[test]
    fn normal_help_excludes_pool_only_actions() {
        let mut app = app_with(&["default"]);
        app.handle(press(KeyCode::Char('?')));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Delete workspace"), "{text}");
        assert!(!text.contains("Send to Worker"), "{text}");
    }

    #[test]
    fn active_pool_editor_folds_refresh_and_renders_worker_lifecycle() {
        let temp = tempfile::tempdir().unwrap();
        let pool = temp.path().join(".jj/pool");
        std::fs::create_dir_all(&pool).unwrap();
        std::fs::write(
            temp.path().join(".jj/pool.json"),
            br#"{"size":1,"gh_repo":"Jarvvski/jjfx","workers":["worker-01"],"created_at":"2026-07-27T10:00:00Z","names":{"worker-01":"alpha"}}"#,
        )
        .unwrap();
        let worker_state = pool.join("worker-01.json");
        std::fs::write(
            &worker_state,
            br#"{"status":"idle","agent":"claude","ticket":null,"pid":null,"started_at":null,"completed_at":null,"log_file":null,"branch_name":null,"exit_code":null,"error":null}"#,
        )
        .unwrap();
        let repository = wsg_core::Repository::open(temp.path()).unwrap();
        let idle = repository.read_worker_pool_snapshot();
        let mut app = app_with(&["default", "worker-01"]);
        app.handle(Msg::WorkspaceDispatch(WorkspaceDispatchEvent::Snapshot {
            operation: 0,
            snapshot: idle,
        }));
        app.handle(press(KeyCode::Char('p')));
        app.handle(press(KeyCode::Char('d')));
        for character in "ENG-101".chars() {
            app.handle(press(KeyCode::Char(character)));
        }

        std::fs::write(
            &worker_state,
            br#"{"status":"busy","agent":"claude","ticket":"ENG-101","pid":null,"started_at":null,"completed_at":null,"log_file":null,"branch_name":"eng-101","exit_code":null,"error":null}"#,
        )
        .unwrap();
        let busy = repository.read_worker_pool_snapshot();
        app.handle(Msg::WorkspaceDispatch(WorkspaceDispatchEvent::Snapshot {
            operation: 0,
            snapshot: busy,
        }));

        assert!(matches!(
            app.mode,
            Mode::Pool(PoolMode::TicketInput { ref buffer, .. }) if buffer == "ENG-101"
        ));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 8)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("worker-01"), "{text}");
        assert!(text.contains("busy"), "{text}");
        assert!(text.contains("ENG-101"), "{text}");
        assert!(text.contains("Ticket IDs: ENG-101"), "{text}");
    }

    #[test]
    fn narrow_workspace_rows_keep_worker_identity_status_and_ticket() {
        let temp = tempfile::tempdir().unwrap();
        let pool = temp.path().join(".jj/pool");
        std::fs::create_dir_all(&pool).unwrap();
        std::fs::write(
            temp.path().join(".jj/pool.json"),
            br#"{"size":1,"gh_repo":"Jarvvski/jjfx","workers":["worker-01"],"created_at":"2026-07-27T10:00:00Z","names":{"worker-01":"alpha"}}"#,
        )
        .unwrap();
        std::fs::write(
            pool.join("worker-01.json"),
            br#"{"status":"busy","agent":"claude","ticket":"ENG-101","pid":null,"started_at":null,"completed_at":null,"log_file":null,"branch_name":null,"exit_code":null,"error":null}"#,
        )
        .unwrap();
        let snapshot = wsg_core::Repository::open(temp.path())
            .unwrap()
            .read_worker_pool_snapshot();
        let mut app = app_with(&["default", "worker-01"]);
        app.handle(Msg::WorkspaceDispatch(WorkspaceDispatchEvent::Snapshot {
            operation: 0,
            snapshot,
        }));

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 8)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("worker-01"), "{text}");
        assert!(text.contains("busy"), "{text}");
        assert!(text.contains("ENG-101"), "{text}");
    }

    #[test]
    fn worker_pool_snapshot_joins_by_workspace_and_renders_worker_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let pool = temp.path().join(".jj/pool");
        std::fs::create_dir_all(&pool).unwrap();
        std::fs::write(
            temp.path().join(".jj/pool.json"),
            br#"{"size":1,"gh_repo":"Jarvvski/jjfx","workers":["worker-01"],"created_at":"2026-07-27T10:00:00Z","names":{"worker-01":"alpha"}}"#,
        )
        .unwrap();
        std::fs::write(
            pool.join("worker-01.json"),
            br#"{"status":"busy","agent":"claude","ticket":"ENG-101","pid":null,"started_at":"2026-07-21T10:00:00Z","completed_at":null,"log_file":"/repo/.jj/pool/worker-01.log","branch_name":"eng-101","exit_code":null,"error":null}"#,
        )
        .unwrap();
        let snapshot = wsg_core::Repository::open(temp.path())
            .unwrap()
            .read_worker_pool_snapshot();

        let mut app = app_with(&["default", "worker-01"]);
        app.handle(Msg::WorkspaceDispatch(WorkspaceDispatchEvent::Snapshot {
            operation: 0,
            snapshot,
        }));

        let worker = app
            .worker_for(app.store.workspace("worker-01").unwrap())
            .expect("Worker should join its Worker Workspace");
        assert_eq!(worker.alias(), "alpha");
        assert_eq!(worker.ticket(), Some("ENG-101"));

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 12)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("alpha"), "{text}");
        assert!(text.contains("ENG-101"), "{text}");
        assert!(text.contains("wsg pool"), "{text}");
    }

    #[test]
    fn capital_t_confirms_before_tidy_and_esc_cancels() {
        let mut app = app_with(&["default"]);
        app.handle(press(KeyCode::Char('T')));
        assert!(matches!(app.mode, Mode::ConfirmTidy));
        app.handle(press(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Normal));
        // The status is untouched by a cancelled tidy (no mutation ran).
        assert!(app.status.is_none());
    }

    #[test]
    fn tidyws_resets_idle_empties_and_reports_the_count() {
        let fake = FakeJj {
            outcome: FakeOutcome {
                tidyws_n: 3,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut app = app_with_jj(&["default"], Box::new(fake.clone()));
        app.handle(press(KeyCode::Char('t')));
        assert_eq!(*fake.tidyws_calls.lock().unwrap(), 1);
        assert_eq!(
            app.status.as_deref(),
            Some("tidyws: reset 3 workspace(s) onto trunk")
        );
    }

    #[test]
    fn tidyws_reports_nothing_to_reset_when_no_match() {
        let fake = FakeJj::default(); // tidyws_n defaults to 0
        let mut app = app_with_jj(&["default"], Box::new(fake.clone()));
        app.handle(press(KeyCode::Char('t')));
        assert_eq!(*fake.tidyws_calls.lock().unwrap(), 1);
        assert_eq!(app.status.as_deref(), Some("tidyws: nothing to reset"));
    }

    #[test]
    fn tidy_runs_only_after_confirmation_and_reports_the_count() {
        let fake = FakeJj {
            outcome: FakeOutcome {
                tidy_n: 2,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut app = app_with_jj(&["default"], Box::new(fake.clone()));
        app.handle(press(KeyCode::Char('T')));
        // The prompt is up but nothing has been abandoned yet.
        assert_eq!(*fake.tidy_calls.lock().unwrap(), 0);
        app.handle(press(KeyCode::Char('y')));
        assert_eq!(*fake.tidy_calls.lock().unwrap(), 1);
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(
            app.status.as_deref(),
            Some("tidy: abandoned 2 junk empty change(s)")
        );
    }

    #[test]
    fn lift_selected_rebases_the_selected_workspace_and_reports() {
        let fake = FakeJj {
            outcome: FakeOutcome {
                lift_ok: true,
                ..Default::default()
            },
            ..Default::default()
        };
        // "default" is the sole workspace, so it is the selection.
        let mut app = app_with_jj(&["default"], Box::new(fake.clone()));
        app.handle(press(KeyCode::Char('r')));
        assert_eq!(*fake.lifted.lock().unwrap(), vec!["default".to_string()]);
        assert_eq!(app.status.as_deref(), Some("lifted default onto trunk"));
    }

    #[test]
    fn lift_selected_reports_nothing_to_lift_when_already_on_trunk() {
        let fake = FakeJj::default(); // lift_ok defaults to false
        let mut app = app_with_jj(&["default"], Box::new(fake.clone()));
        app.handle(press(KeyCode::Char('r')));
        assert_eq!(app.status.as_deref(), Some("default: nothing to lift"));
    }

    #[test]
    fn lift_selected_surfaces_the_jj_error() {
        let fake = FakeJj {
            outcome: FakeOutcome {
                fail: Some("immutable commit".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut app = app_with_jj(&["default"], Box::new(fake.clone()));
        app.handle(press(KeyCode::Char('r')));
        assert_eq!(app.status.as_deref(), Some("lift failed: immutable commit"));
    }

    #[test]
    fn lift_all_rebases_every_workspace_and_reports() {
        let fake = FakeJj {
            outcome: FakeOutcome {
                lift_all_ok: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut app = app_with_jj(&["default", "feat"], Box::new(fake.clone()));
        app.handle(press(KeyCode::Char('R')));
        assert_eq!(*fake.lift_all_calls.lock().unwrap(), 1);
        assert_eq!(
            app.status.as_deref(),
            Some("lifted all workspaces onto trunk")
        );
    }

    #[test]
    fn status_clears_when_its_expiry_fires() {
        let fake = FakeJj {
            outcome: FakeOutcome {
                lift_ok: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut app = app_with_jj(&["default"], Box::new(fake));
        app.handle(press(KeyCode::Char('r')));
        assert!(app.status.is_some());
        app.handle(Msg::StatusExpired(app.status_gen));
        assert!(app.status.is_none());
    }

    #[test]
    fn stale_expiry_leaves_a_newer_status_alone() {
        let fake = FakeJj {
            outcome: FakeOutcome {
                lift_ok: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut app = app_with_jj(&["default"], Box::new(fake));
        app.handle(press(KeyCode::Char('r')));
        let stale = app.status_gen;
        // A second action replaces the message before the first timer fires.
        app.handle(press(KeyCode::Char('R')));
        assert_eq!(app.status.as_deref(), Some("nothing to lift"));
        app.handle(Msg::StatusExpired(stale));
        assert_eq!(app.status.as_deref(), Some("nothing to lift"));
    }

    #[tokio::test]
    async fn u_starts_one_background_fetch_and_folds_its_outcome() {
        let mut app = app_with(&["default"]);
        app.handle(press(KeyCode::Char('u')));
        assert_eq!(app.status.as_deref(), Some("fetching…"));
        // A second press while one is in flight is ignored (repo-lock contention).
        app.handle(press(KeyCode::Char('u')));
        assert_eq!(app.status.as_deref(), Some("fetching…"));

        app.handle(Msg::Fetched(Ok(())));
        assert_eq!(app.status.as_deref(), Some("fetched"));
        // Resolved: the next press may fetch again.
        app.handle(press(KeyCode::Char('u')));
        assert_eq!(app.status.as_deref(), Some("fetching…"));
    }

    #[test]
    fn fetch_failure_surfaces_the_jj_error_in_the_footer() {
        let mut app = app_with(&["default"]);
        app.handle(Msg::Fetched(Err("remote unreachable".into())));
        assert_eq!(
            app.status.as_deref(),
            Some("fetch failed: remote unreachable")
        );
    }

    #[test]
    fn submitted_workspace_name_creates_and_opens_terminal() {
        let repo = store::test_local_repo("app-create");
        let terminal = FakeTerminal::default();
        let mut app = app_with_store(Store::load(&repo), Box::new(terminal.clone()));

        app.handle(press(KeyCode::Char('n')));
        for character in "feat".chars() {
            app.handle(press(KeyCode::Char(character)));
        }
        app.handle(press(KeyCode::Enter));

        assert_eq!(
            terminal.opened.lock().unwrap().as_slice(),
            &[("feat".to_string(), true)]
        );
        assert_eq!(app.status.as_deref(), Some("created 'feat'"));
        let fresh = Store::load(&repo);
        let path = fresh.workspace("feat").unwrap().path.clone().unwrap();
        assert_eq!(
            fresh.workspace("feat").unwrap().path.as_deref(),
            Some(path.as_path())
        );

        std::fs::remove_dir_all(path).unwrap();
        std::fs::remove_dir_all(repo).unwrap();
    }

    #[tokio::test]
    async fn configured_workspace_creation_waits_for_command_before_opening() {
        let repo = store::test_local_repo("app-create-on-create");
        let terminal = FakeTerminal::default();
        let (mut app, mut rx) = app_with_store_and_workspace_config(
            Store::load(&repo),
            Box::new(terminal.clone()),
            waiting_workspace_config(),
        );

        submit_new_workspace(&mut app, "feat");

        assert!(terminal.opened.lock().unwrap().is_empty());
        assert_eq!(app.status.as_deref(), Some("configuring 'feat'..."));
        let path = app.store.workspace("feat").unwrap().path.clone().unwrap();
        std::fs::write(path.join("on-create-release"), "").unwrap();

        handle_on_create_completion(&mut app, &mut rx).await;

        assert_eq!(
            terminal.opened.lock().unwrap().as_slice(),
            &[("feat".to_string(), true)]
        );
        assert_eq!(app.status.as_deref(), Some("created 'feat'"));

        std::fs::remove_dir_all(path).unwrap();
        std::fs::remove_dir_all(repo).unwrap();
    }

    #[tokio::test]
    async fn a_second_creation_is_refused_while_on_create_is_running() {
        let repo = store::test_local_repo("app-create-on-create-busy");
        let terminal = FakeTerminal::default();
        let (mut app, mut rx) = app_with_store_and_workspace_config(
            Store::load(&repo),
            Box::new(terminal),
            waiting_workspace_config(),
        );

        submit_new_workspace(&mut app, "feat");
        app.handle(press(KeyCode::Char('n')));

        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(
            app.status.as_deref(),
            Some("workspace 'feat' is still configuring")
        );

        let path = app.store.workspace("feat").unwrap().path.clone().unwrap();
        std::fs::write(path.join("on-create-release"), "").unwrap();
        handle_on_create_completion(&mut app, &mut rx).await;

        std::fs::remove_dir_all(path).unwrap();
        std::fs::remove_dir_all(repo).unwrap();
    }

    #[tokio::test]
    async fn failed_on_create_keeps_workspace_closed_and_reports_stderr() {
        let repo = store::test_local_repo("app-create-on-create-failure");
        let terminal = FakeTerminal::default();
        let (mut app, mut rx) = app_with_store_and_workspace_config(
            Store::load(&repo),
            Box::new(terminal.clone()),
            workspace_config(&["sh", "-c", "printf 'setup boom' >&2; exit 7"]),
        );

        submit_new_workspace(&mut app, "feat");
        handle_on_create_completion(&mut app, &mut rx).await;

        assert!(app.store.workspace("feat").is_some());
        assert!(app.pending_workspace.is_none());
        assert!(terminal.opened.lock().unwrap().is_empty());
        let status = app.status.as_deref().unwrap();
        assert!(status.starts_with("created 'feat', on-create failed:"));
        assert!(status.contains("setup boom"), "{status}");

        let path = app.store.workspace("feat").unwrap().path.clone().unwrap();
        std::fs::remove_dir_all(path).unwrap();
        std::fs::remove_dir_all(repo).unwrap();
    }

    #[tokio::test]
    async fn successful_on_create_reports_terminal_open_failure() {
        let repo = store::test_local_repo("app-create-on-create-tab-failure");
        let terminal = FakeTerminal::default();
        *terminal.open_error.lock().unwrap() = Some("kitty unavailable".to_string());
        let (mut app, mut rx) = app_with_store_and_workspace_config(
            Store::load(&repo),
            Box::new(terminal),
            workspace_config(&["true"]),
        );

        submit_new_workspace(&mut app, "feat");
        handle_on_create_completion(&mut app, &mut rx).await;

        assert_eq!(
            app.status.as_deref(),
            Some("created 'feat', tab failed: kitty unavailable")
        );

        let path = app.store.workspace("feat").unwrap().path.clone().unwrap();
        std::fs::remove_dir_all(path).unwrap();
        std::fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn create_failure_opens_no_terminal() {
        let repo = store::test_local_repo("app-create-failure");
        let store = Store::load(&repo);
        let path = repo.with_file_name(format!(
            "{}-external-feat",
            repo.file_name().unwrap().to_string_lossy()
        ));
        jj::add_workspace(&repo, "feat", &path).unwrap();
        let terminal = FakeTerminal::default();
        let mut app = app_with_store(store, Box::new(terminal.clone()));

        app.handle(press(KeyCode::Char('n')));
        for character in "feat".chars() {
            app.handle(press(KeyCode::Char(character)));
        }
        app.handle(press(KeyCode::Enter));

        assert!(app.status.as_deref().unwrap().starts_with("create failed:"));
        assert!(terminal.opened.lock().unwrap().is_empty());

        std::fs::remove_dir_all(path).unwrap();
        std::fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn confirmed_delete_closes_terminal_and_removes_workspace() {
        let repo = store::test_local_repo("app-delete");
        let mut store = Store::load(&repo);
        store.create("feat").unwrap();
        let terminal = FakeTerminal::default();
        let mut app = app_with_store(store, Box::new(terminal.clone()));

        app.handle(press(KeyCode::Down));
        app.handle(press(KeyCode::Char('d')));
        app.handle(press(KeyCode::Char('y')));

        assert_eq!(terminal.closed.lock().unwrap().as_slice(), &["feat"]);
        assert!(Store::load(&repo).workspace("feat").is_none());

        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn selection_moves_and_clamps() {
        // Default is pinned, then the idle group is sorted by name: a, b.
        let mut app = app_with(&["default", "a", "b"]);
        assert_eq!(app.list.selected(), Some("default"));
        app.handle(press(KeyCode::Up)); // clamp at top
        assert_eq!(app.list.selected(), Some("default"));
        app.handle(press(KeyCode::Down));
        assert_eq!(app.list.selected(), Some("a"));
        app.handle(press(KeyCode::Down));
        assert_eq!(app.list.selected(), Some("b"));
        app.handle(press(KeyCode::Down));
        assert_eq!(app.list.selected(), Some("b")); // clamp at bottom
    }

    #[test]
    fn list_groups_by_attention_needs_you_first() {
        use crate::work::Work;
        let mut app = app_with(&["default", "busy", "blocked", "dirtyws"]);
        // Give each workspace a distinct axis so they land in distinct groups.
        // canon() no-ops on the nonexistent /wt/* paths, so agent keys match.
        app.handle(Msg::AgentEvent(agent::Event {
            name: "PermissionRequest".into(),
            cwd: "/wt/blocked".into(),
            transcript_path: None,
        }));
        app.handle(Msg::AgentEvent(agent::Event {
            name: "UserPromptSubmit".into(),
            cwd: "/wt/busy".into(),
            transcript_path: None,
        }));
        let mut snap = HashMap::new();
        snap.insert(
            "dirtyws".to_string(),
            Work {
                state: WorkState::Dirty {
                    added: 1,
                    removed: 0,
                },
                behind: 0,
            },
        );
        app.handle(Msg::WorkSnapshot(snap));

        // Group order via classified(): needs-you, working, ready-to-forge, idle.
        let groups: Vec<Attention> = app.classified().iter().map(|(a, _)| *a).collect();
        assert_eq!(groups[0], Attention::NeedsYou); // blocked
        assert_eq!(groups[1], Attention::Working); // busy
        assert_eq!(groups[2], Attention::ReadyToForge); // dirtyws
        assert_eq!(groups[3], Attention::Idle); // default
    }

    #[test]
    fn default_workspace_renders_before_all_attention_groups() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = app_with(&["default", "blocked"]);
        app.handle(Msg::AgentEvent(agent::Event {
            name: "PermissionRequest".into(),
            cwd: "/wt/blocked".into(),
            transcript_path: None,
        }));

        let mut term = Terminal::new(TestBackend::new(120, 12)).unwrap();
        term.draw(|frame| app.render(frame)).unwrap();
        let width = term.backend().buffer().area.width as usize;
        let lines: Vec<String> = term
            .backend()
            .buffer()
            .content()
            .chunks(width)
            .map(|cells| cells.iter().map(|cell| cell.symbol()).collect())
            .collect();
        let default_line = lines
            .iter()
            .position(|line| line.contains("default"))
            .unwrap();
        let needs_you_line = lines
            .iter()
            .position(|line| line.contains("needs you (1)"))
            .unwrap();

        assert!(default_line < needs_you_line);
    }

    #[test]
    fn idle_group_folds_and_selection_stays_valid() {
        let mut app = app_with(&["default", "a"]); // both idle
        assert_eq!(app.selectable_names().len(), 2);
        // Fold idle -> pinned default remains visible and selected.
        app.handle(press(KeyCode::Char('c')));
        assert!(app.list.idle_collapsed());
        assert_eq!(app.selectable_names(), vec!["default"]);
        assert_eq!(app.list.selected(), Some("default"));
        // Unfold restores the grouped workspace without disturbing selection.
        app.handle(press(KeyCode::Char('c')));
        assert!(!app.list.idle_collapsed());
        assert_eq!(app.selectable_names().len(), 2);
        assert_eq!(app.list.selected(), Some("default"));
    }

    /// Two changed files with distinct magnitudes, folded in via `Msg::DiffLoaded`.
    fn detail_files() -> Vec<FileDiff> {
        use crate::diff::{DiffLine, LineKind};
        vec![
            FileDiff {
                path: "src/app.rs".to_string(),
                added: 2,
                removed: 1,
                lines: vec![
                    DiffLine {
                        kind: LineKind::Added,
                        text: "let x = 1;".to_string(),
                    },
                    DiffLine {
                        kind: LineKind::Added,
                        text: "let y = 2;".to_string(),
                    },
                    DiffLine {
                        kind: LineKind::Removed,
                        text: "old".to_string(),
                    },
                ],
            },
            FileDiff {
                path: "README.md".to_string(),
                added: 1,
                removed: 0,
                lines: vec![DiffLine {
                    kind: LineKind::Added,
                    text: "hi".to_string(),
                }],
            },
        ]
    }

    /// Enter Detail directly (bypassing the async spawn in `open_detail`) and
    /// populate it, mirroring what the loaded diff snapshot does.
    fn app_in_detail(ws: &str) -> App {
        let mut app = app_with(&["default", ws]);
        app.list.select(ws);
        app.mode = Mode::Detail(Detail::loading(ws.to_string()));
        app.handle(Msg::DiffLoaded {
            ws: ws.to_string(),
            files: detail_files(),
        });
        app
    }

    #[tokio::test]
    async fn right_opens_detail_for_the_selected_workspace() {
        let mut app = app_with(&["default", "feat"]);
        app.list.select("feat");
        // `l` (and `→`) drills into the diff viewer for the selected workspace;
        // the diff itself loads on a background task. (The loading/populate
        // behaviour is tested directly against `Detail` in `diff_view`.)
        app.handle(press(KeyCode::Char('l')));
        match &app.mode {
            Mode::Detail(d) => assert_eq!(d.workspace(), "feat"),
            _ => panic!("expected Detail mode"),
        }
    }

    #[test]
    fn esc_and_left_from_files_close_the_detail_view() {
        let mut app = app_in_detail("feat");
        app.handle(press(KeyCode::Left));
        assert!(matches!(app.mode, Mode::Normal));

        let mut app = app_in_detail("feat");
        app.handle(press(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn detail_renders_without_panicking() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = app_in_detail("feat");
        // A tiny terminal must clamp, not panic.
        let mut term = Terminal::new(TestBackend::new(8, 4)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        // A roomy one draws the full two-pane layout.
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
    }

    /// A small graph: trunk `t1`, a `default` chain `t1 -> c1 -> c2(@)`, and a
    /// `feat` chain with a child past `@`.
    fn sample_graph() -> graph::Graph {
        let node =
            |id: &str, parents: &[&str], wc_of: &[&str], immutable: bool, ts: i64| graph::Node {
                id: id.to_string(),
                change_id: format!("ch{id}"),
                summary: format!("summary for {id}"),
                parents: parents.iter().map(|p| p.to_string()).collect(),
                bookmarks: vec![],
                timestamp_ms: ts,
                wc_of: wc_of.iter().map(|w| w.to_string()).collect(),
                immutable,
            };
        let nodes = [
            node("t1", &[], &[], true, 1),
            node("c1", &["t1"], &[], false, 2),
            node("c2", &["c1"], &["default"], false, 3),
            node("f1", &["t1"], &["feat"], false, 4),
            node("f2", &["f1"], &[], false, 5),
        ]
        .into_iter()
        .map(|n| (n.id.clone(), n))
        .collect();
        graph::Graph {
            nodes,
            trunk_id: Some("t1".to_string()),
            chains: vec![
                graph::Chain {
                    workspace: "default".to_string(),
                    head: "c2".to_string(),
                    commits: vec!["c2".to_string(), "c1".to_string()],
                    base: Some("t1".to_string()),
                    child: None,
                },
                graph::Chain {
                    workspace: "feat".to_string(),
                    head: "f1".to_string(),
                    commits: vec!["f1".to_string()],
                    base: Some("t1".to_string()),
                    child: Some("f2".to_string()),
                },
            ],
        }
    }

    #[tokio::test]
    async fn w_toggles_the_inline_world_pane() {
        let mut app = app_with(&["default"]);
        assert!(!app.world_pane());
        app.handle(press(KeyCode::Char('w')));
        assert!(app.world_pane(), "w turns the pane on");
        assert!(matches!(app.mode, Mode::Normal), "no mode change");
        app.handle(press(KeyCode::Char('w')));
        assert!(!app.world_pane(), "w again turns it off");
    }

    #[tokio::test]
    async fn shift_w_opens_and_closes_the_full_screen_graph() {
        let mut app = app_with(&["default"]);
        app.handle(press(KeyCode::Char('W')));
        assert!(matches!(app.mode, Mode::Graph(_)));
        app.handle(press(KeyCode::Char('W')));
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn app_starts_with_a_persisted_on_world_pane() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let app = App::new(
            Store::from_workspaces_for_test(PathBuf::from("/repo"), vec![]),
            agent::AgentStates::default(),
            Box::new(FakeTerminal::default()),
            Box::new(FakeJj::default()),
            AppConfig::default(),
            true,
            tx,
        );
        assert!(app.world_pane());
    }

    #[test]
    fn home_view_renders_the_world_pane_when_toggled_on() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = app_with(&["default", "feat"]);
        app.world = Some(Viewport::default());
        app.graph = Some(sample_graph());

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("world"), "the pane's title is drawn");
        assert!(
            text.contains("summary for t1"),
            "the graph's content is drawn"
        );

        // A tiny terminal drops the pane rather than corrupting the layout.
        let mut term = Terminal::new(TestBackend::new(20, 6)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
    }

    #[test]
    fn home_view_world_pane_shows_loading_before_the_graph_arrives() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = app_with(&["default"]);
        app.world = Some(Viewport::default());
        // graph is still None: must render a loading state, not panic.
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("loading"));
    }

    #[test]
    fn shift_j_and_k_scroll_the_world_pane_only_when_it_is_on() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = app_with(&["default", "feat"]);
        // Pane off: J/K are inert (and must not panic).
        app.handle(press(KeyCode::Char('J')));
        app.handle(press(KeyCode::Char('K')));
        assert!(app.world.is_none());

        app.world = Some(Viewport::default());
        app.graph = Some(sample_graph());
        // A short terminal gives the pane fewer rows than the graph has lines
        // (9 for the sample), so there is room to scroll once the render has
        // recorded the geometry.
        let mut term = Terminal::new(TestBackend::new(100, 16)).unwrap();
        term.draw(|f| app.render(f)).unwrap();

        app.handle(press(KeyCode::Char('J')));
        assert_eq!(app.world.unwrap().scroll(), 1, "J scrolls the pane down");
        app.handle(press(KeyCode::Char('K')));
        assert_eq!(app.world.unwrap().scroll(), 0, "K scrolls it back up");
        // The list selection never moved: J/K drive the pane, j/k the list.
        assert_eq!(app.list.selected(), Some("default"));
    }

    #[test]
    fn world_graph_renders_narrow_and_wide_without_panicking() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = app_with(&["default", "feat"]);
        app.list.select("feat");
        app.mode = Mode::Graph(Viewport::default());
        app.graph = Some(sample_graph());

        // A tiny terminal must clamp its layout, not corrupt or panic (AC 4).
        let mut term = Terminal::new(TestBackend::new(6, 3)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        // A roomy one draws the full graph.
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
    }

    #[test]
    fn world_graph_shows_loading_before_the_graph_arrives() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = app_with(&["default"]);
        app.mode = Mode::Graph(Viewport::default());
        // graph is still None: must render a loading state, not panic.
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
    }

    #[test]
    fn detail_graph_strip_appears_only_when_wide_enough() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = app_in_detail("feat");
        app.graph = Some(sample_graph());

        // Wide: files + diff + strip all fit.
        let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        // Narrow: the strip is dropped so the diff stays readable (no panic).
        let mut term = Terminal::new(TestBackend::new(70, 30)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
    }

    fn live(state: AgentState, kind: AgentKind) -> agent::Agent {
        agent::Agent { state, kind }
    }

    #[test]
    fn claude_frames_bounce_and_codex_frames_wrap() {
        // Claude's petals bloom out and close back: 0 1 2 3 4 5 4 3 2 1, then
        // tick 10 starts the next bloom at frame 0 again.
        let seq: String = (0..=10)
            .map(|t| working_frame(AgentKind::Claude, t))
            .collect();
        assert_eq!(seq, "❀✼✴✳✛•✛✳✴✼❀");
        // Codex's hexagon fills slice by slice, then restarts empty.
        let seq: String = (0..=6)
            .map(|t| working_frame(AgentKind::Codex, t))
            .collect();
        assert_eq!(
            seq,
            "\u{f0ac3}\u{f0ac4}\u{f0ac5}\u{f0ac6}\u{f0ac7}\u{f0ac8}\u{f0ac3}"
        );
    }

    #[test]
    fn agent_glyph_matches_state_and_kind() {
        use AgentKind::*;
        use AgentState::*;
        // Each agent pauses under its own mark; colour carries the state.
        assert_eq!(agent_glyph(live(Waiting, Claude), 0).content, "✻ ");
        assert_eq!(agent_glyph(live(Waiting, Codex), 0).content, "\u{f02d9} ");
        assert_eq!(
            agent_glyph(live(NeedsAttention, Codex), 0).content,
            "\u{f02d9} "
        );
        assert_eq!(
            agent_glyph(live(NeedsAttention, Codex), 0).style.fg,
            Some(Color::Red)
        );
        assert_eq!(agent_glyph(live(Absent, Unknown), 0).content, "· ");
        assert_eq!(agent_glyph(live(Ended, Claude), 0).content, "· ");
        // Each agent works in its own animation and brand colour.
        assert_eq!(agent_glyph(live(Working, Claude), 0).content, "❀ ");
        assert_eq!(agent_glyph(live(Working, Codex), 0).content, "\u{f0ac3} ");
        assert_eq!(
            agent_glyph(live(Working, Claude), 0).style.fg,
            Some(CLAUDE_ORANGE)
        );
        assert_eq!(
            agent_glyph(live(Working, Codex), 0).style.fg,
            Some(CODEX_CYAN)
        );
    }

    #[test]
    fn agent_label_names_the_agent_only_while_live() {
        use AgentKind::*;
        use AgentState::*;
        assert_eq!(agent_label(live(Working, Claude)), "claude");
        assert_eq!(agent_label(live(Waiting, Codex)), "codex");
        assert_eq!(agent_label(live(NeedsAttention, Unknown)), "agent");
        // No live session: a dash, whoever was here before.
        assert_eq!(agent_label(live(Absent, Unknown)), "-");
        assert_eq!(agent_label(live(Ended, Claude)), "-");
    }

    #[test]
    fn ticks_redraw_only_while_an_agent_works_on_screen() {
        let mut app = app_with(&["feat"]);
        // No live agent: ticks advance the frame but warrant no repaint.
        assert!(!app.animate());

        // A turn starts in the workspace (cwd matches its path): animate.
        for name in ["SessionStart", "UserPromptSubmit"] {
            app.handle(Msg::AgentEvent(agent::Event {
                name: name.to_string(),
                cwd: "/wt/feat".to_string(),
                transcript_path: None,
            }));
        }
        assert!(app.animate());

        // A full-screen view hides the list, so the glyph has nothing to move.
        app.mode = Mode::Graph(Viewport::default());
        assert!(!app.animate());
        app.mode = Mode::Normal;

        // The turn ends: back to a static glyph, no repaint per tick.
        app.handle(Msg::AgentEvent(agent::Event {
            name: "Stop".to_string(),
            cwd: "/wt/feat".to_string(),
            transcript_path: None,
        }));
        assert!(!app.animate());
    }
}
