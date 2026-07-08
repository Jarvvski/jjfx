//! Application state and the update/render logic. Background tasks send [`Msg`]
//! over a channel to the single owned `App`, which the main loop mutates and
//! redraws (the engine shape from the PRD).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};
use tokio::sync::mpsc::UnboundedSender;

use crate::agent::{self, AgentState};
use crate::attention::{self, Attention};
use crate::forge::{self, Target};
use crate::store::{self, Store, Workspace};
use crate::terminal::Terminal;
use crate::work::{Work, WorkState};
use crate::{cache, jj};

/// What the key handler is currently collecting: normal navigation, a new
/// workspace name, or a delete confirmation.
enum Mode {
    Normal,
    NewWorkspace(String),
    ConfirmDelete(String),
    /// Confirming the destructive `tidy` sweep (abandon junk empties).
    ConfirmTidy,
}

/// One rendered list line: a group header (non-selectable) or a workspace row.
enum Row<'a> {
    Header(Attention, usize),
    Ws(&'a Workspace, Attention),
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
    /// A forge pipeline transition (ticket 08).
    Forge(forge::Update),
}

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
    /// Current agent lifecycle state per workspace, keyed by canonicalized path
    /// (the `cwd` join from the hook log). Absent workspaces are simply missing.
    agents: HashMap<PathBuf, AgentState>,
    /// Latest work-lifecycle snapshot per workspace, keyed by workspace name.
    /// Missing entries render as unknown until the first snapshot arrives.
    work: HashMap<String, Work>,
    /// Live forge progress per workspace, keyed by name. An entry exists only
    /// while a forge runs or after one that ended with a skip/failure.
    forge: HashMap<String, ForgeProgress>,
    /// Channel to the app's own message loop, so forge tasks can stream updates
    /// back as [`Msg::Forge`].
    tx: UnboundedSender<Msg>,
    /// The multiplexer jjfx drives for workspace tabs (behind a trait so kitty is
    /// swappable - ticket 07).
    terminal: Box<dyn Terminal>,
    mode: Mode,
    /// A transient one-line message shown in the footer (last action's result).
    status: Option<String>,
    /// Selection tracked by workspace name, not row index, so it follows a
    /// workspace as live state re-sorts it between Attention groups.
    selected: Option<String>,
    /// Whether the idle group is folded away.
    idle_collapsed: bool,
    /// Render-only: the highlighted row index, recomputed from `selected` each
    /// draw (the list interleaves non-selectable group headers).
    list_state: ListState,
    pub should_quit: bool,
}

impl App {
    pub fn new(
        store: Store,
        agents: HashMap<PathBuf, AgentState>,
        terminal: Box<dyn Terminal>,
        tx: UnboundedSender<Msg>,
    ) -> Self {
        let mut app = App {
            store,
            agents,
            work: HashMap::new(),
            forge: HashMap::new(),
            tx,
            terminal,
            mode: Mode::Normal,
            status: None,
            selected: None,
            idle_collapsed: false,
            list_state: ListState::default(),
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
        }
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
                    self.status = Some(format!("{ws}: {r}"));
                }
            }
            forge::Update::Skip { ws, reason } => {
                let entry = self.forge.entry(ws.clone()).or_default();
                entry.active = false;
                entry.reason = Some(reason.clone());
                self.status = Some(format!("{ws}: {reason}"));
            }
            forge::Update::Done { ws } => {
                if let Some(entry) = self.forge.get_mut(&ws) {
                    entry.active = false;
                    // A clean run leaves nothing to show; the advanced work state
                    // (picked up by the poller) speaks for itself.
                    if entry.clean_success() {
                        self.forge.remove(&ws);
                        self.status = Some(format!("{ws}: forged"));
                    }
                }
            }
            forge::Update::Aborted(reason) => {
                // Drop every still-running overlay; the run did no per-ws work.
                self.forge.retain(|_, p| !p.active);
                self.status = Some(format!("forge aborted: {reason}"));
            }
        }
    }

    /// Fold one hook event into the per-workspace agent state.
    fn on_agent_event(&mut self, ev: agent::Event) {
        let key = agent::canon(std::path::Path::new(&ev.cwd));
        let entry = self.agents.entry(key).or_insert(AgentState::Absent);
        *entry = agent::transition(*entry, &ev.name);
    }

    /// The agent state for a workspace, `Absent` if the log has none for it.
    fn agent_state(&self, w: &Workspace) -> AgentState {
        w.path
            .as_deref()
            .map(agent::canon)
            .and_then(|p| self.agents.get(&p).copied())
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
            KeyCode::Char('d') => self.begin_delete_selected(),
            KeyCode::Char('t') => self.tidyws(),
            KeyCode::Char('T') => self.begin_tidy(),
            KeyCode::Char('f') => self.forge_selected(),
            KeyCode::Char('F') => self.forge_all(),
            KeyCode::Char('g') => self.forge_default(),
            KeyCode::Char('c') => {
                self.idle_collapsed = !self.idle_collapsed;
                self.ensure_selection();
            }
            _ => {}
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
        self.selected
            .as_ref()
            .and_then(|s| self.store.workspaces.iter().find(|w| &w.name == s))
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
            self.status = Some(format!("no path known for '{}'", w.name));
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
            self.status = Some(format!("open failed: {e}"));
        }
    }

    /// `d`: confirm before deleting; the default workspace is undeletable.
    fn begin_delete_selected(&mut self) {
        self.status = None;
        let Some(w) = self.selected_workspace() else {
            return;
        };
        if w.name == store::DEFAULT_WORKSPACE {
            self.status = Some("the default workspace cannot be deleted".to_string());
            return;
        }
        self.mode = Mode::ConfirmDelete(w.name.clone());
    }

    /// Create a workspace: `jj workspace add`, persist its chosen path to the
    /// ws-cache (jj records no path), reload, then open its tab.
    fn create_workspace(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            self.status = Some("workspace name required".to_string());
            return;
        }
        if self.store.workspaces.iter().any(|w| w.name == name) {
            self.status = Some(format!("workspace '{name}' already exists"));
            return;
        }
        let path = store::new_workspace_path(&self.store.repo_root, name);
        if let Err(e) = jj::add_workspace(&self.store.repo_root, name, &path) {
            self.status = Some(format!("create failed: {e}"));
            return;
        }
        if let Err(e) = self.persist_cache_add(name, &path) {
            self.status = Some(format!("cache write failed: {e}"));
        }
        self.reload();
        match self.terminal.open(name, &path, true) {
            Ok(()) => self.status = Some(format!("created '{name}'")),
            Err(e) => self.status = Some(format!("created '{name}', tab failed: {e}")),
        }
    }

    /// Delete a workspace: close its tab, `jj workspace forget`, remove its
    /// directory (guarded - never the repo root), drop it from the cache, reload.
    fn delete_workspace(&mut self, name: &str) {
        if name == store::DEFAULT_WORKSPACE {
            self.status = Some("the default workspace cannot be deleted".to_string());
            return;
        }
        let path = self
            .store
            .workspaces
            .iter()
            .find(|w| w.name == name)
            .and_then(|w| w.path.clone());

        let _ = self.terminal.close(name); // best-effort; jj is the source of truth
        if let Err(e) = jj::forget_workspace(&self.store.repo_root, name) {
            self.status = Some(format!("delete failed: {e}"));
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
        self.status = Some(format!("deleted '{name}'"));
    }

    /// `t`: reset idle, empty, undescribed workspace working-copies onto latest
    /// `trunk()`. Non-destructive (workspaces with real work are untouched), so it
    /// runs without confirmation; the poller refreshes each row's `behind` count.
    fn tidyws(&mut self) {
        self.status = Some(match jj::tidyws(&self.store.repo_root) {
            Ok(0) => "tidyws: nothing to reset".to_string(),
            Ok(n) => format!("tidyws: reset {n} workspace(s) onto trunk"),
            Err(e) => format!("tidyws failed: {e}"),
        });
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
        self.status = Some(match jj::tidy(&self.store.repo_root) {
            Ok(0) => "tidy: nothing to abandon".to_string(),
            Ok(n) => format!("tidy: abandoned {n} junk empty change(s)"),
            Err(e) => format!("tidy failed: {e}"),
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
            self.status = Some(match skipped_no_path {
                Some(name) => format!("no path known for '{name}'"),
                None => "nothing to forge".to_string(),
            });
            return;
        }
        let tx = self.tx.clone();
        let repo_root = self.store.repo_root.clone();
        tokio::spawn(async move { forge::run(tx, repo_root, targets).await });
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

    fn move_selection(&mut self, delta: isize) {
        let names: Vec<String> = self.selectable().iter().map(|w| w.name.clone()).collect();
        if names.is_empty() {
            self.selected = None;
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|s| names.iter().position(|n| n == s))
            .unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, names.len() as isize - 1);
        self.selected = Some(names[next as usize].clone());
    }

    /// Re-reconcile from disk; the selection follows its workspace by name.
    fn reload(&mut self) {
        self.store = Store::load(&self.store.repo_root);
        self.ensure_selection();
    }

    /// Point the selection at a real, currently-selectable workspace, falling
    /// back to the first one when the current target is gone or hidden.
    fn ensure_selection(&mut self) {
        let names: Vec<String> = self.selectable().iter().map(|w| w.name.clone()).collect();
        let valid = self.selected.as_ref().is_some_and(|s| names.contains(s));
        if !valid {
            self.selected = names.into_iter().next();
        }
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

    /// The display rows: a header per non-empty group, then its workspace rows
    /// (unless the idle group is collapsed).
    fn rows(&self) -> Vec<Row<'_>> {
        let classified = self.classified();
        let mut rows = Vec::new();
        let mut idx = 0;
        while idx < classified.len() {
            let att = classified[idx].0;
            let end = idx
                + classified[idx..]
                    .iter()
                    .take_while(|(a, _)| *a == att)
                    .count();
            rows.push(Row::Header(att, end - idx));
            if !(att == Attention::Idle && self.idle_collapsed) {
                for (a, w) in &classified[idx..end] {
                    rows.push(Row::Ws(w, *a));
                }
            }
            idx = end;
        }
        rows
    }

    /// The currently-selectable workspaces, in display order (excludes ones
    /// hidden in a collapsed idle group).
    fn selectable(&self) -> Vec<&Workspace> {
        self.rows()
            .into_iter()
            .filter_map(|r| match r {
                Row::Ws(w, _) => Some(w),
                Row::Header(..) => None,
            })
            .collect()
    }

    /// Render the Attention-grouped workspace list plus a header and footer.
    pub fn render(&mut self, frame: &mut Frame) {
        self.ensure_selection();

        let [header, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(frame.area());

        let title = format!(" jjfx - {} workspace(s) ", self.store.workspaces.len());
        frame.render_widget(
            Paragraph::new(Span::styled(
                title,
                Style::default().add_modifier(Modifier::BOLD),
            )),
            header,
        );

        // Build list items from the grouped rows, tracking which item index the
        // selected workspace lands on so the highlight follows it.
        let selected = self.selected.clone();
        let mut selected_idx = None;
        let items: Vec<ListItem> = self
            .rows()
            .into_iter()
            .enumerate()
            .map(|(i, row)| match row {
                Row::Header(att, count) => self.header_item(att, count),
                Row::Ws(w, att) => {
                    if selected.as_deref() == Some(w.name.as_str()) {
                        selected_idx = Some(i);
                    }
                    self.workspace_item(w, att)
                }
            })
            .collect();

        self.list_state.select(selected_idx);
        let list = List::new(items)
            .block(Block::bordered())
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, body, &mut self.list_state);

        frame.render_widget(self.footer(), footer);
    }

    /// A group-header row: the Attention heading, count, and a fold hint for idle.
    fn header_item(&self, att: Attention, count: usize) -> ListItem<'static> {
        let mut text = format!("{} ({count})", att.heading());
        if att == Attention::Idle {
            text.push_str(if self.idle_collapsed {
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
    fn workspace_item(&self, w: &Workspace, att: Attention) -> ListItem<'static> {
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
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(
                format!("{:<10}", att.heading()),
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
        spans.push(Span::raw(format!("{:<18} {}", w.name, path)));
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
                    " j/k move  n new  enter open  o open-bg  d delete  f forge  F all  g default  t tidyws  T tidy  c fold-idle  q quit ",
                    Style::default().add_modifier(Modifier::DIM),
                )),
            },
        }
    }
}

fn display_path(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
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
/// step (`f w p s`), each coloured by its live status.
fn forge_spans(progress: &ForgeProgress) -> Vec<Span<'static>> {
    use crate::forge::Step;
    let mut spans = vec![Span::styled("⚒ ", Style::default().fg(Color::Magenta))];
    for step in [Step::Fetch, Step::Weld, Step::Push, Step::Spr] {
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

    fn app_with(names: &[&str]) -> App {
        app_with_terminal(names, Box::new(FakeTerminal::default()))
    }

    fn app_with_terminal(names: &[&str], terminal: Box<dyn Terminal>) -> App {
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
            HashMap::new(),
            terminal,
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
        for step in [Step::Fetch, Step::Weld, Step::Push, Step::Spr] {
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
    fn selection_moves_and_clamps() {
        // All idle -> one group, sorted by name: a, b, default.
        let mut app = app_with(&["default", "a", "b"]);
        assert_eq!(app.selected.as_deref(), Some("a"));
        app.handle(press(KeyCode::Up)); // clamp at top
        assert_eq!(app.selected.as_deref(), Some("a"));
        app.handle(press(KeyCode::Down));
        assert_eq!(app.selected.as_deref(), Some("b"));
        app.handle(press(KeyCode::Down));
        app.handle(press(KeyCode::Down)); // clamp at bottom
        assert_eq!(app.selected.as_deref(), Some("default"));
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
        assert_eq!(app.selectable().len(), 2);
        // Fold idle -> no selectable rows remain, selection clears gracefully.
        app.handle(press(KeyCode::Char('c')));
        assert!(app.idle_collapsed);
        assert_eq!(app.selectable().len(), 0);
        assert_eq!(app.selected, None);
        // Unfold restores selectability and a valid selection.
        app.handle(press(KeyCode::Char('c')));
        assert!(!app.idle_collapsed);
        assert_eq!(app.selectable().len(), 2);
        assert!(app.selected.is_some());
    }
}
