//! Application state and the update/render logic. Background tasks send [`Msg`]
//! over a channel to the single owned `App`, which the main loop mutates and
//! redraws (the engine shape from the PRD).

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ratatui::Frame;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, ListItem, Paragraph};
use tokio::sync::mpsc::UnboundedSender;

use crate::agent::{self, AgentState};
use crate::attention::{self, Attention};
use crate::config::ForgeConfig;
use crate::diff::{self, FileDiff};
use crate::diff_view::Detail;
use crate::forge::{self, Target};
use crate::graph;
use crate::store::{self, Store, Workspace};
use crate::terminal::Terminal;
use crate::viewport::Viewport;
use crate::work::{Work, WorkState};
use crate::workspace_list::{Row, WorkspaceList};
use crate::{cache, jj};

/// What the key handler is currently collecting: normal navigation, a new
/// workspace name, or a delete confirmation.
enum Mode {
    Normal,
    NewWorkspace(String),
    ConfirmDelete(String),
    /// Confirming the destructive `tidy` sweep (abandon junk empties).
    ConfirmTidy,
    /// The `?` help overlay is open (a pure UI mode - no state is mutated).
    Help,
    /// The full-screen diff-detail view for one workspace (ADR 0008).
    Detail(Detail),
    /// The full-screen "world" commit graph: trunk plus every workspace's chain
    /// (ticket 11). The rendered lines are rebuilt each draw from `App::graph`,
    /// so only the [`Viewport`] offset is held here.
    Graph(Viewport),
}

/// Every keybinding, shown in the `?` help overlay. Kept adjacent to
/// `on_key_normal` (the real dispatch) so the list cannot silently drift from
/// the keys the app actually handles.
const BINDINGS: &[(&str, &str)] = &[
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
    ("Fold / expand idle group", "c"),
    ("Toggle this help", "?"),
    ("Quit", "q / esc"),
];

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
    /// A forge pipeline transition (ticket 08).
    Forge(forge::Update),
    /// The background `jj git fetch` finished; `Err` carries jj's error text.
    Fetched(Result<(), String>),
    /// The diff for a workspace finished loading (ticket 10).
    DiffLoaded { ws: String, files: Vec<FileDiff> },
    /// The commit graph finished loading from jj-lib (ticket 11).
    GraphLoaded(graph::Graph),
    /// A footer status message's expiry timer fired. Carries the generation it
    /// was armed for; a stale one (an action replaced the message since) is a
    /// no-op.
    StatusExpired(u64),
}

/// How long a transient footer status message stays before expiring.
const STATUS_TTL: Duration = Duration::from_secs(5);

/// The live forge progress for one workspace: the four steps' statuses, whether a
/// pipeline is still running, and the last skip/abort reason (for the footer).
#[derive(Default)]
struct ForgeProgress {
    steps: [Option<forge::Status>; 4],
    active: bool,
    reason: Option<String>,
}

impl ForgeProgress {
    /// A clean success: finished with every step OK and no skip reason. Such rows
    /// drop their progress overlay and revert to the (now-advanced) work state.
    fn clean_success(&self) -> bool {
        !self.active
            && self.reason.is_none()
            && self.steps.iter().all(|s| *s == Some(forge::Status::Ok))
    }
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
    /// Live forge progress per workspace, keyed by name. An entry exists only
    /// while a forge runs or after one that ended with a skip/failure.
    forge: HashMap<String, ForgeProgress>,
    /// A background `jj git fetch` is in flight; a second `u` is ignored until
    /// it resolves (two would just contend on the repo lock).
    fetching: bool,
    /// How the forge manages pull requests (toggle + draft), handed to each
    /// [`forge::run`].
    forge_config: ForgeConfig,
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
    pub should_quit: bool,
}

impl App {
    pub fn new(
        store: Store,
        agents: agent::AgentStates,
        terminal: Box<dyn Terminal>,
        jj: Box<dyn jj::Jj>,
        forge_config: ForgeConfig,
        world_pane: bool,
        tx: UnboundedSender<Msg>,
    ) -> Self {
        let mut app = App {
            store,
            agents,
            work: HashMap::new(),
            forge: HashMap::new(),
            fetching: false,
            forge_config,
            tx,
            terminal,
            jj,
            mode: Mode::Normal,
            graph: None,
            world: world_pane.then(Viewport::default),
            status: None,
            status_gen: 0,
            list: WorkspaceList::default(),
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
            Msg::Forge(update) => self.on_forge(update),
            Msg::Fetched(result) => self.on_fetched(result),
            Msg::DiffLoaded { ws, files } => self.on_diff_loaded(ws, files),
            Msg::GraphLoaded(graph) => self.graph = Some(graph),
            Msg::StatusExpired(generation) => {
                if generation == self.status_gen {
                    self.status = None;
                }
            }
        }
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

    /// Fold one forge transition into per-workspace progress and the footer.
    fn on_forge(&mut self, update: forge::Update) {
        match update {
            forge::Update::Start(names) => {
                self.status = None;
                for name in names {
                    self.forge.insert(
                        name,
                        ForgeProgress {
                            active: true,
                            ..Default::default()
                        },
                    );
                }
            }
            forge::Update::Step {
                ws,
                step,
                status,
                reason,
            } => {
                let entry = self.forge.entry(ws.clone()).or_default();
                entry.active = true;
                entry.steps[step.index()] = Some(status);
                if let Some(r) = reason {
                    entry.reason = Some(r.clone());
                    self.set_status(format!("{ws}: {r}"));
                }
            }
            forge::Update::Skip { ws, reason } => {
                let entry = self.forge.entry(ws.clone()).or_default();
                entry.active = false;
                entry.reason = Some(reason.clone());
                self.set_status(format!("{ws}: {reason}"));
            }
            forge::Update::Done { ws } => {
                if let Some(entry) = self.forge.get_mut(&ws) {
                    entry.active = false;
                    // A clean run leaves nothing to show; the advanced work state
                    // (picked up by the poller) speaks for itself.
                    if entry.clean_success() {
                        self.forge.remove(&ws);
                        self.set_status(format!("{ws}: forged"));
                    }
                }
                // A forge moves revisions (weld/push); refresh the graph if shown.
                self.refresh_graph_if_visible();
            }
            forge::Update::Aborted(reason) => {
                // Drop every still-running overlay; the run did no per-ws work.
                self.forge.retain(|_, p| !p.active);
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
        w.path
            .as_deref()
            .map(|p| self.agents.state_for(p))
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
        match &self.mode {
            Mode::Normal => self.on_key_normal(key),
            Mode::NewWorkspace(_) => self.on_key_new_workspace(key),
            Mode::ConfirmDelete(_) => self.on_key_confirm_delete(key),
            Mode::ConfirmTidy => self.on_key_confirm_tidy(key),
            Mode::Help => self.on_key_help(key),
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
            KeyCode::Char('c') => {
                self.list.toggle_idle();
                self.ensure_selection();
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            _ => {}
        }
    }

    /// Help is a read-only overlay: `?` or `esc` dismisses it, everything else
    /// is swallowed so no navigation leaks through.
    fn on_key_help(&mut self, key: KeyEvent) {
        if matches!(key.code, KeyCode::Char('?') | KeyCode::Esc) {
            self.mode = Mode::Normal;
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
        let repo_root = self.store.repo_root.clone();
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
        let repo_root = self.store.repo_root.clone();
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
            .and_then(|s| self.store.workspaces.iter().find(|w| w.name == s))
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
        let Some(w) = self.selected_workspace() else {
            return;
        };
        if w.name == store::DEFAULT_WORKSPACE {
            self.set_status("the default workspace cannot be deleted".to_string());
            return;
        }
        self.mode = Mode::ConfirmDelete(w.name.clone());
    }

    /// Create a workspace: `jj workspace add`, persist its chosen path to the
    /// ws-cache (jj records no path), reload, then open its tab.
    fn create_workspace(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            self.set_status("workspace name required".to_string());
            return;
        }
        if self.store.workspaces.iter().any(|w| w.name == name) {
            self.set_status(format!("workspace '{name}' already exists"));
            return;
        }
        let path = store::new_workspace_path(&self.store.repo_root, name);
        if let Err(e) = self.jj.add_workspace(name, &path) {
            self.set_status(format!("create failed: {e}"));
            return;
        }
        if let Err(e) = self.persist_cache_add(name, &path) {
            self.set_status(format!("cache write failed: {e}"));
        }
        self.reload();
        match self.terminal.open(name, &path, true) {
            Ok(()) => self.set_status(format!("created '{name}'")),
            Err(e) => self.set_status(format!("created '{name}', tab failed: {e}")),
        }
    }

    /// Delete a workspace: close its tab, `jj workspace forget`, remove its
    /// directory (guarded - never the repo root), drop it from the cache, reload.
    fn delete_workspace(&mut self, name: &str) {
        if name == store::DEFAULT_WORKSPACE {
            self.set_status("the default workspace cannot be deleted".to_string());
            return;
        }
        let path = self
            .store
            .workspaces
            .iter()
            .find(|w| w.name == name)
            .and_then(|w| w.path.clone());

        let _ = self.terminal.close(name); // best-effort; jj is the source of truth
        if let Err(e) = self.jj.forget_workspace(name) {
            self.set_status(format!("delete failed: {e}"));
            return;
        }
        if let Some(p) = path
            && p != self.store.repo_root
            && p.is_dir()
        {
            let _ = std::fs::remove_dir_all(&p);
        }
        let _ = self.persist_cache_remove(name);
        self.reload();
        self.set_status(format!("deleted '{name}'"));
    }

    /// `t`: reset idle, empty, undescribed workspace working-copies onto latest
    /// `trunk()`. Non-destructive (workspaces with real work are untouched), so it
    /// runs without confirmation; the poller refreshes each row's `behind` count.
    fn tidyws(&mut self) {
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
        let repo_root = self.store.repo_root.clone();
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
            self.start_forge(vec![w]);
        }
    }

    /// `g`: forge the default workspace.
    fn forge_default(&mut self) {
        if let Some(w) = self
            .store
            .workspaces
            .iter()
            .find(|w| w.name == store::DEFAULT_WORKSPACE)
            .cloned()
        {
            self.start_forge(vec![w]);
        }
    }

    /// `F`: forge every eligible workspace, sequentially (in one background run).
    fn forge_all(&mut self) {
        let all: Vec<Workspace> = self.store.workspaces.clone();
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
            if self.forge.get(&w.name).is_some_and(|p| p.active) {
                continue; // already forging
            }
            match &w.path {
                Some(dir) => targets.push(Target {
                    name: w.name.clone(),
                    dir: dir.clone(),
                }),
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
        let tx = self.tx.clone();
        let repo_root = self.store.repo_root.clone();
        let cfg = self.forge_config;
        tokio::spawn(async move { forge::run(tx, repo_root, targets, cfg).await });
    }

    /// Upsert a `(name, path)` into the ws-cache so the path jj does not record
    /// survives a reload.
    fn persist_cache_add(&self, name: &str, path: &Path) -> std::io::Result<()> {
        let cache_path = cache::path(&self.store.repo_root);
        let mut entries = cache::read(&cache_path).unwrap_or_default();
        if !entries.iter().any(|(n, _)| n == name) {
            entries.push((name.to_string(), path.to_path_buf()));
        }
        cache::write_through(&cache_path, &entries)?;
        Ok(())
    }

    /// Drop a workspace from the ws-cache.
    fn persist_cache_remove(&self, name: &str) -> std::io::Result<()> {
        let cache_path = cache::path(&self.store.repo_root);
        let entries: Vec<_> = cache::read(&cache_path)
            .unwrap_or_default()
            .into_iter()
            .filter(|(n, _)| n != name)
            .collect();
        cache::write_through(&cache_path, &entries)?;
        Ok(())
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
        self.store = Store::load(&self.store.repo_root);
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
            .workspaces
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

        if matches!(self.mode, Mode::Detail(_)) {
            self.render_detail(frame);
            return;
        }
        if matches!(self.mode, Mode::Graph(_)) {
            self.render_graph_world(frame);
            return;
        }

        let [header, body, footer] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .horizontal_margin(2)
        .areas(frame.area());

        let title = format!("jjfx - {} workspace(s)", self.store.workspaces.len());
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
        let rows = self.list.rows(&classified);
        let mut cursor = None;
        let items: Vec<ListItem> = rows
            .iter()
            .enumerate()
            .map(|(i, row)| match row {
                Row::Header(att, count) => self.header_item(*att, *count),
                Row::Ws(w, att) => {
                    let is_selected = self.list.selected() == Some(w.name.as_str());
                    if is_selected {
                        cursor = Some(i);
                    }
                    self.workspace_item(w, *att, is_selected)
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

        if matches!(self.mode, Mode::Help) {
            self.render_help(frame);
        }
    }

    /// The `?` overlay: a centered, bordered box listing every binding
    /// (label-left, key-right) drawn over a dimmed copy of the list behind it.
    fn render_help(&self, frame: &mut Frame) {
        let label_w = BINDINGS
            .iter()
            .map(|(label, _)| label.chars().count())
            .max()
            .unwrap_or(0);
        let key_w = BINDINGS
            .iter()
            .map(|(_, key)| key.chars().count())
            .max()
            .unwrap_or(0);

        let lines: Vec<Line> = BINDINGS
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
        let height = BINDINGS.len() as u16 + 2;
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
                    "  trunk + every workspace",
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

    /// The world graph's rendered lines - trunk plus every workspace's chain,
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
    fn workspace_item(&self, w: &Workspace, att: Attention, selected: bool) -> ListItem<'static> {
        let agent = self.agent_state(w);
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
            Span::styled(
                format!("{:<11}", agent.label()),
                Style::default().fg(agent_color(agent)),
            ),
        ];
        // While a forge is running (or ended with a skip), its live pipeline
        // takes the work column; otherwise the work label shows there.
        match self.forge.get(&w.name) {
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
        ListItem::new(Line::from(spans))
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
            Mode::Normal => match &self.status {
                Some(msg) => Paragraph::new(Span::styled(
                    format!(" {msg} "),
                    Style::default().fg(Color::Yellow),
                )),
                None => Paragraph::new(Span::styled(
                    if self.world.is_some() {
                        " j/k move  J/K scroll world  ? help  q quit "
                    } else {
                        " j/k move  ? help  q quit "
                    },
                    Style::default().add_modifier(Modifier::DIM),
                )),
            },
            // Help draws its own overlay; the footer stays on the slim hint.
            Mode::Help => Paragraph::new(Span::styled(
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

/// The world view: trunk (plus a little context) then each workspace's chain,
/// the selected chain highlighted. Chains hang off trunk with `├─`/`╰─`
/// connectors; commits stack directly under their branch header for compactness.
fn world_graph_lines(
    g: &graph::Graph,
    selected: Option<&str>,
    now_ms: i64,
    width: u16,
) -> Vec<Line<'static>> {
    let w = width.saturating_sub(2);
    let mut lines = Vec::new();

    if let Some(tid) = &g.trunk_id {
        if let Some(node) = g.nodes.get(tid) {
            lines.push(commit_line(
                "",
                "● ",
                Color::Cyan,
                node,
                false,
                false,
                now_ms,
                w,
            ));
        }
        for id in g.trunk_context.iter().skip(1).take(3) {
            if let Some(node) = g.nodes.get(id) {
                lines.push(commit_line(
                    "│ ",
                    "○ ",
                    Color::DarkGray,
                    node,
                    false,
                    false,
                    now_ms,
                    w,
                ));
            }
        }
    }
    if !g.chains.is_empty() {
        lines.push(dim_line("│"));
    }

    let last = g.chains.len().saturating_sub(1);
    for (i, chain) in g.chains.iter().enumerate() {
        let is_sel = selected == Some(chain.workspace.as_str());
        let (conn, prefix) = if i == last {
            ("╰─ ", "   ")
        } else {
            ("├─ ", "│  ")
        };
        let name_style = if is_sel {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };
        let mut header = vec![
            Span::raw(conn.to_string()),
            Span::styled(chain.workspace.clone(), name_style),
        ];
        if chain.commits.is_empty() && chain.child.is_none() {
            header.push(Span::styled(
                "  on trunk",
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
        lines.push(Line::from(header));

        if let Some(cid) = &chain.child
            && let Some(node) = g.nodes.get(cid)
        {
            let mut l = commit_line(prefix, "◆ ", Color::Yellow, node, false, is_sel, now_ms, w);
            l.spans.push(Span::styled(
                "  +1",
                Style::default().add_modifier(Modifier::DIM),
            ));
            lines.push(l);
        }
        for id in &chain.commits {
            if let Some(node) = g.nodes.get(id) {
                let is_head = id == &chain.head;
                let glyph = if is_head { "● " } else { "○ " };
                let color = if is_sel { Color::Cyan } else { Color::Gray };
                lines.push(commit_line(
                    prefix, glyph, color, node, is_head, is_sel, now_ms, w,
                ));
            }
        }
        if i != last {
            lines.push(dim_line("│"));
        }
    }
    lines
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

/// Colour cue for an agent state - drawing the eye to what is live or blocked.
fn agent_color(state: AgentState) -> Color {
    match state {
        AgentState::Absent => Color::DarkGray,
        AgentState::Working => Color::Green,
        AgentState::Waiting => Color::Yellow,
        AgentState::NeedsAttention => Color::Red,
        AgentState::Ended => Color::DarkGray,
    }
}

/// The compact forge pipeline for a row: a `⚒` sigil then one `letter+glyph` per
/// step (`f w p r`), each coloured by its live status.
fn forge_spans(progress: &ForgeProgress) -> Vec<Span<'static>> {
    use crate::forge::Step;
    let mut spans = vec![Span::styled("⚒ ", Style::default().fg(Color::Magenta))];
    for step in [Step::Fetch, Step::Weld, Step::Push, Step::Pr] {
        let status = progress.steps[step.index()];
        spans.push(Span::styled(
            format!("{}{} ", step.letter(), forge_glyph(status)),
            Style::default().fg(forge_color(status)),
        ));
    }
    spans
}

/// Glyph for a forge step's status: pending, running, done, or skipped.
fn forge_glyph(status: Option<forge::Status>) -> char {
    match status {
        None => '·',
        Some(forge::Status::Running) => '…',
        Some(forge::Status::Ok) => '✓',
        Some(forge::Status::Skipped) => '~',
    }
}

/// Colour for a forge step's status.
fn forge_color(status: Option<forge::Status>) -> Color {
    match status {
        None => Color::DarkGray,
        Some(forge::Status::Running) => Color::Cyan,
        Some(forge::Status::Ok) => Color::Green,
        Some(forge::Status::Skipped) => Color::Yellow,
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
    }

    impl Terminal for FakeTerminal {
        fn is_open(&self, name: &str) -> bool {
            self.existing.lock().unwrap().iter().any(|n| n == name)
        }
        fn open(&self, name: &str, _path: &Path, focus: bool) -> anyhow::Result<()> {
            self.opened.lock().unwrap().push((name.to_string(), focus));
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
        added: Arc<Mutex<Vec<(String, PathBuf)>>>,
        forgotten: Arc<Mutex<Vec<String>>>,
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
        fn add_workspace(&self, name: &str, dest: &Path) -> anyhow::Result<()> {
            self.added
                .lock()
                .unwrap()
                .push((name.to_string(), dest.to_path_buf()));
            self.result(())
        }
        fn forget_workspace(&self, name: &str) -> anyhow::Result<()> {
            self.forgotten.lock().unwrap().push(name.to_string());
            self.result(())
        }
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
            Store {
                repo_root: Path::new("/repo").to_path_buf(),
                workspaces,
            },
            agent::AgentStates::default(),
            terminal,
            jj,
            ForgeConfig::default(),
            false,
            tx,
        )
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
    fn q_and_esc_request_quit() {
        let mut app = app_with(&["default"]);
        app.handle(press(KeyCode::Char('q')));
        assert!(app.should_quit);

        let mut app = app_with(&["default"]);
        app.handle(press(KeyCode::Esc));
        assert!(app.should_quit);
    }

    #[test]
    fn help_toggles_without_touching_state() {
        let mut app = app_with(&["default", "feat"]);
        // Move selection off the top so we can prove Help leaves it untouched.
        app.handle(press(KeyCode::Down));
        let before = app.list.selected().map(str::to_string);

        app.handle(press(KeyCode::Char('?')));
        assert!(matches!(app.mode, Mode::Help));
        // Navigation is swallowed while the overlay is open, and no status leaks.
        app.handle(press(KeyCode::Down));
        assert!(matches!(app.mode, Mode::Help));
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
        app.mode = Mode::Help;
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
        app.mode = Mode::Help;
        let mut term = Terminal::new(TestBackend::new(6, 3)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
    }

    #[test]
    fn bindings_cover_the_essential_keys() {
        let keys: Vec<&str> = BINDINGS.iter().map(|(_, k)| *k).collect();
        assert!(keys.contains(&"j / ↓"));
        assert!(keys.contains(&"?"));
        assert!(keys.contains(&"q / esc"));
        assert!(
            BINDINGS
                .iter()
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
    fn agent_event_updates_the_matching_workspace_row() {
        let mut app = app_with(&["default", "feat"]);
        // canon() no-ops on nonexistent paths, so /wt/feat matches the row path.
        app.handle(Msg::AgentEvent(agent::Event {
            name: "UserPromptSubmit".into(),
            cwd: "/wt/feat".into(),
        }));
        let feat = app
            .store
            .workspaces
            .iter()
            .find(|w| w.name == "feat")
            .unwrap();
        assert_eq!(app.agent_state(feat), AgentState::Working);
        // A workspace with no events stays Absent.
        let def = app
            .store
            .workspaces
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
            .workspaces
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
            .workspaces
            .iter()
            .find(|w| w.name == "default")
            .unwrap();
        assert_eq!(app.work_state(def), WorkState::Unknown);
        assert_eq!(app.behind(def), 0);
    }

    #[test]
    fn forge_updates_fold_into_row_progress_and_footer() {
        use crate::forge::{Status, Step, Update};
        let mut app = app_with(&["default", "feat"]);

        // Start marks the workspace as forging with an empty pipeline.
        app.handle(Msg::Forge(Update::Start(vec!["feat".to_string()])));
        assert!(app.forge.get("feat").is_some_and(|p| p.active));

        // A running then ok step is recorded in the right slot.
        app.handle(Msg::Forge(Update::Step {
            ws: "feat".to_string(),
            step: Step::Weld,
            status: Status::Ok,
            reason: None,
        }));
        assert_eq!(
            app.forge.get("feat").unwrap().steps[Step::Weld.index()],
            Some(Status::Ok)
        );

        // A skip carries its reason to the footer.
        app.handle(Msg::Forge(Update::Step {
            ws: "feat".to_string(),
            step: Step::Push,
            status: Status::Skipped,
            reason: Some("push: nothing to push".to_string()),
        }));
        assert_eq!(app.status.as_deref(), Some("feat: push: nothing to push"));

        // Done on a run that had a skip keeps the overlay visible (not clean).
        app.handle(Msg::Forge(Update::Done {
            ws: "feat".to_string(),
        }));
        assert!(app.forge.contains_key("feat"));
        assert!(!app.forge.get("feat").unwrap().active);
    }

    #[test]
    fn clean_forge_run_drops_the_overlay() {
        use crate::forge::{Status, Step, Update};
        let mut app = app_with(&["feat"]);
        app.handle(Msg::Forge(Update::Start(vec!["feat".to_string()])));
        for step in [Step::Fetch, Step::Weld, Step::Push, Step::Pr] {
            app.handle(Msg::Forge(Update::Step {
                ws: "feat".to_string(),
                step,
                status: Status::Ok,
                reason: None,
            }));
        }
        app.handle(Msg::Forge(Update::Done {
            ws: "feat".to_string(),
        }));
        // All steps OK, no skip: the overlay clears so the row shows work state.
        assert!(!app.forge.contains_key("feat"));
        assert_eq!(app.status.as_deref(), Some("feat: forged"));
    }

    #[test]
    fn forge_abort_clears_active_overlays_with_a_reason() {
        use crate::forge::Update;
        let mut app = app_with(&["feat"]);
        app.handle(Msg::Forge(Update::Start(vec!["feat".to_string()])));
        app.handle(Msg::Forge(Update::Aborted("GPG key locked".to_string())));
        assert!(!app.forge.contains_key("feat"));
        assert_eq!(app.status.as_deref(), Some("forge aborted: GPG key locked"));
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
    fn create_workspace_adds_via_jj_and_opens_the_tab() {
        let term = FakeTerminal::default();
        let fake = FakeJj::default();
        let mut app = app_full(&["default"], Box::new(term.clone()), Box::new(fake.clone()));
        app.create_workspace("feat");
        // jj adds the workspace at the derived sibling path, with the chosen name.
        assert_eq!(
            *fake.added.lock().unwrap(),
            vec![("feat".to_string(), PathBuf::from("/repo-feat"))]
        );
        // The tab opens with focus, and the footer confirms.
        assert_eq!(
            term.opened.lock().unwrap().as_slice(),
            &[("feat".to_string(), true)]
        );
        assert_eq!(app.status.as_deref(), Some("created 'feat'"));
    }

    #[test]
    fn create_workspace_surfaces_the_jj_error_and_opens_no_tab() {
        let term = FakeTerminal::default();
        let fake = FakeJj {
            outcome: FakeOutcome {
                fail: Some("path exists".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut app = app_full(&["default"], Box::new(term.clone()), Box::new(fake.clone()));
        app.create_workspace("feat");
        assert_eq!(app.status.as_deref(), Some("create failed: path exists"));
        assert!(term.opened.lock().unwrap().is_empty());
    }

    #[test]
    fn delete_workspace_forgets_via_jj_closes_the_tab_and_reports() {
        let term = FakeTerminal::default();
        let fake = FakeJj::default();
        let mut app = app_full(
            &["default", "feat"],
            Box::new(term.clone()),
            Box::new(fake.clone()),
        );
        app.delete_workspace("feat");
        assert_eq!(*fake.forgotten.lock().unwrap(), vec!["feat".to_string()]);
        assert_eq!(
            term.closed.lock().unwrap().as_slice(),
            &["feat".to_string()]
        );
        assert_eq!(app.status.as_deref(), Some("deleted 'feat'"));
    }

    #[test]
    fn selection_moves_and_clamps() {
        // All idle -> one group, sorted by name: a, b, default.
        let mut app = app_with(&["default", "a", "b"]);
        assert_eq!(app.list.selected(), Some("a"));
        app.handle(press(KeyCode::Up)); // clamp at top
        assert_eq!(app.list.selected(), Some("a"));
        app.handle(press(KeyCode::Down));
        assert_eq!(app.list.selected(), Some("b"));
        app.handle(press(KeyCode::Down));
        app.handle(press(KeyCode::Down)); // clamp at bottom
        assert_eq!(app.list.selected(), Some("default"));
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
        }));
        app.handle(Msg::AgentEvent(agent::Event {
            name: "UserPromptSubmit".into(),
            cwd: "/wt/busy".into(),
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
    fn idle_group_folds_and_selection_stays_valid() {
        let mut app = app_with(&["default", "a"]); // both idle
        assert_eq!(app.selectable_names().len(), 2);
        // Fold idle -> no selectable rows remain, selection clears gracefully.
        app.handle(press(KeyCode::Char('c')));
        assert!(app.list.idle_collapsed());
        assert_eq!(app.selectable_names().len(), 0);
        assert_eq!(app.list.selected(), None);
        // Unfold restores selectability and a valid selection.
        app.handle(press(KeyCode::Char('c')));
        assert!(!app.list.idle_collapsed());
        assert_eq!(app.selectable_names().len(), 2);
        assert!(app.list.selected().is_some());
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
        let node = |id: &str| graph::Node {
            id: id.to_string(),
            change_id: format!("ch{id}"),
            summary: format!("summary for {id}"),
            parents: vec![],
            bookmarks: vec![],
            timestamp_ms: 0,
            wc_of: vec![],
        };
        let nodes = ["t1", "c1", "c2", "f1", "f2"]
            .iter()
            .map(|id| (id.to_string(), node(id)))
            .collect();
        graph::Graph {
            nodes,
            trunk_id: Some("t1".to_string()),
            trunk_context: vec!["t1".to_string()],
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
            Store {
                repo_root: Path::new("/repo").to_path_buf(),
                workspaces: vec![],
            },
            agent::AgentStates::default(),
            Box::new(FakeTerminal::default()),
            Box::new(FakeJj::default()),
            ForgeConfig::default(),
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
}
