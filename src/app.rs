//! Application state and the update/render logic. Background tasks send [`Msg`]
//! over a channel to the single owned `App`, which the main loop mutates and
//! redraws (the engine shape from the PRD).

use std::collections::HashMap;
use std::path::PathBuf;

use ratatui::Frame;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};

use crate::agent::{self, AgentState};
use crate::store::{Store, Workspace};

/// Messages folded into the app from the terminal and background watchers.
#[derive(Debug)]
pub enum Msg {
    /// A terminal input event (key, resize, ...).
    Input(Event),
    /// The repo changed on disk; re-reconcile the store.
    Reload,
    /// A Claude Code hook event, appended to the global log (ADR 0004).
    AgentEvent(agent::Event),
}

/// The whole application state - one owned value on the main task.
pub struct App {
    store: Store,
    /// Current agent lifecycle state per workspace, keyed by canonicalized path
    /// (the `cwd` join from the hook log). Absent workspaces are simply missing.
    agents: HashMap<PathBuf, AgentState>,
    list_state: ListState,
    pub should_quit: bool,
}

impl App {
    pub fn new(store: Store, agents: HashMap<PathBuf, AgentState>) -> Self {
        let mut list_state = ListState::default();
        if !store.workspaces.is_empty() {
            list_state.select(Some(0));
        }
        App {
            store,
            agents,
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

    fn on_key(&mut self, key: KeyEvent) {
        // Only react to presses; crossterm can also deliver Release/Repeat.
        if key.kind == KeyEventKind::Release {
            return;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            _ => {}
        }
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
                let state = self.agent_state(w);
                let path = w
                    .path
                    .as_deref()
                    .map(display_path)
                    .unwrap_or_else(|| "(path unknown - not in ws-cache)".to_string());
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{:<11}", state.label()),
                        Style::default().fg(agent_color(state)),
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

        frame.render_widget(
            Paragraph::new(Span::styled(
                " j/k or ↑/↓ move   q/esc quit ",
                Style::default().add_modifier(Modifier::DIM),
            )),
            footer,
        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Workspace;
    use ratatui::crossterm::event::{KeyEventState, KeyModifiers};
    use std::path::{Path, PathBuf};

    fn app_with(names: &[&str]) -> App {
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
