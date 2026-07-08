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

use crate::agent::{self, AgentState};
use crate::store::{self, Store, Workspace};
use crate::terminal::Terminal;
use crate::work::WorkState;
use crate::{cache, jj};

/// What the key handler is currently collecting: normal navigation, a new
/// workspace name, or a delete confirmation.
enum Mode {
    Normal,
    NewWorkspace(String),
    ConfirmDelete(String),
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
    WorkSnapshot(HashMap<String, WorkState>),
}

/// The whole application state - one owned value on the main task.
pub struct App {
    store: Store,
    /// Current agent lifecycle state per workspace, keyed by canonicalized path
    /// (the `cwd` join from the hook log). Absent workspaces are simply missing.
    agents: HashMap<PathBuf, AgentState>,
    /// Latest work-lifecycle state per workspace, keyed by workspace name.
    /// Missing entries render as unknown until the first snapshot arrives.
    work: HashMap<String, WorkState>,
    /// The multiplexer jjfx drives for workspace tabs (behind a trait so kitty is
    /// swappable - ticket 07).
    terminal: Box<dyn Terminal>,
    mode: Mode,
    /// A transient one-line message shown in the footer (last action's result).
    status: Option<String>,
    list_state: ListState,
    pub should_quit: bool,
}

impl App {
    pub fn new(
        store: Store,
        agents: HashMap<PathBuf, AgentState>,
        terminal: Box<dyn Terminal>,
    ) -> Self {
        let mut list_state = ListState::default();
        if !store.workspaces.is_empty() {
            list_state.select(Some(0));
        }
        App {
            store,
            agents,
            work: HashMap::new(),
            terminal,
            mode: Mode::Normal,
            status: None,
            list_state,
            should_quit: false,
        }
    }

    /// Fold one message into the state.
    pub fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Input(Event::Key(key)) => self.on_key(key),
            Msg::Input(_) => {}
            Msg::Reload => self.reload(),
            Msg::AgentEvent(ev) => self.on_agent_event(ev),
            Msg::WorkSnapshot(work) => self.work = work,
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
        self.work.get(&w.name).copied().unwrap_or_default()
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

    /// The workspace under the cursor, if any.
    fn selected_workspace(&self) -> Option<&Workspace> {
        self.list_state
            .selected()
            .and_then(|i| self.store.workspaces.get(i))
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
        let len = self.store.workspaces.len();
        if len == 0 {
            return;
        }
        let current = self.list_state.selected().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, len as isize - 1);
        self.list_state.select(Some(next as usize));
    }

    /// Re-reconcile from disk, preserving the selection where possible.
    fn reload(&mut self) {
        self.store = Store::load(&self.store.repo_root);
        let len = self.store.workspaces.len();
        match len {
            0 => self.list_state.select(None),
            _ => {
                let clamped = self.list_state.selected().unwrap_or(0).min(len - 1);
                self.list_state.select(Some(clamped));
            }
        }
    }

    /// Render the workspace list plus a header and a key-hint footer.
    pub fn render(&mut self, frame: &mut Frame) {
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

        let items: Vec<ListItem> = self
            .store
            .workspaces
            .iter()
            .map(|w| {
                let agent = self.agent_state(w);
                let work = self.work_state(w);
                let path = w
                    .path
                    .as_deref()
                    .map(display_path)
                    .unwrap_or_else(|| "(path unknown - not in ws-cache)".to_string());
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{:<11}", agent.label()),
                        Style::default().fg(agent_color(agent)),
                    ),
                    Span::styled(
                        format!("{:<16}", work.label()),
                        Style::default().fg(work_color(work)),
                    ),
                    Span::raw(format!("{:<20} {}", w.name, path)),
                ]))
            })
            .collect();

        let list = List::new(items)
            .block(Block::bordered())
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, body, &mut self.list_state);

        frame.render_widget(self.footer(), footer);
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
            Mode::Normal => match &self.status {
                Some(msg) => Paragraph::new(Span::styled(
                    format!(" {msg} "),
                    Style::default().fg(Color::Yellow),
                )),
                None => Paragraph::new(Span::styled(
                    " j/k move  n new  enter open  o open-bg  d delete  q quit ",
                    Style::default().add_modifier(Modifier::DIM),
                )),
            },
        }
    }
}

fn display_path(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
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
        App::new(
            Store {
                repo_root: Path::new("/repo").to_path_buf(),
                workspaces,
            },
            HashMap::new(),
            terminal,
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
        use crate::work::WorkState;
        let mut app = app_with(&["default", "feat"]);
        let mut snap = HashMap::new();
        snap.insert(
            "feat".to_string(),
            WorkState::Dirty {
                added: 9,
                removed: 2,
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
        // A workspace absent from the snapshot stays Unknown.
        let def = app
            .store
            .workspaces
            .iter()
            .find(|w| w.name == "default")
            .unwrap();
        assert_eq!(app.work_state(def), WorkState::Unknown);
    }

    #[test]
    fn selection_moves_and_clamps() {
        let mut app = app_with(&["default", "a", "b"]);
        assert_eq!(app.list_state.selected(), Some(0));
        app.handle(press(KeyCode::Up)); // clamp at top
        assert_eq!(app.list_state.selected(), Some(0));
        app.handle(press(KeyCode::Down));
        app.handle(press(KeyCode::Down));
        app.handle(press(KeyCode::Down)); // clamp at bottom (len 3)
        assert_eq!(app.list_state.selected(), Some(2));
    }
}
