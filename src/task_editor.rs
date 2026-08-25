use ratatui::Frame;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Position};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui_textarea::{Input, Key, TextArea};

/// The result of handling one task-editor input event.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TaskEditorAction {
    /// Keep the editor open.
    Continue,
    /// Close the editor without sending anything.
    Cancel,
    /// Close the editor and send this prompt.
    Submit(String),
}

/// Full-screen multiline task prompt editor.
pub(crate) struct TaskEditor {
    target: String,
    textarea: TextArea<'static>,
    error: Option<String>,
    scroll: u16,
}

impl TaskEditor {
    /// Open an empty task editor for the displayed target.
    pub(crate) fn new(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            textarea: TextArea::default(),
            error: None,
            scroll: 0,
        }
    }

    /// Handle one terminal key event.
    pub(crate) fn on_key(&mut self, key: KeyEvent) -> TaskEditorAction {
        if key.kind == KeyEventKind::Release {
            return TaskEditorAction::Continue;
        }
        if key.code == KeyCode::Esc {
            return TaskEditorAction::Cancel;
        }
        if key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::SHIFT) {
            let prompt = self.prompt();
            if prompt.trim().is_empty() {
                self.error = Some("Task prompt cannot be empty".to_owned());
                return TaskEditorAction::Continue;
            }
            return TaskEditorAction::Submit(prompt);
        }

        if self.textarea.input(input_from_key(key)) {
            self.error = None;
        }
        TaskEditorAction::Continue
    }

    /// Handle a terminal event, including bracketed multiline paste.
    pub(crate) fn on_event(&mut self, event: Event) -> TaskEditorAction {
        match event {
            Event::Key(key) => self.on_key(key),
            Event::Paste(text) => {
                if self.textarea.insert_str(text) {
                    self.error = None;
                }
                TaskEditorAction::Continue
            }
            _ => TaskEditorAction::Continue,
        }
    }

    /// Return the current prompt text as it would be submitted.
    pub(crate) fn prompt(&self) -> String {
        self.textarea.lines().join("\n")
    }

    /// Render the editor across the whole terminal page.
    pub(crate) fn render(&mut self, frame: &mut Frame) {
        let [editor, footer] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
        let inner_height = editor.height.saturating_sub(2);
        self.keep_cursor_visible(inner_height);

        let lines = self.render_lines();
        let title = format!(" Send task to {} ", self.target);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(title);
        frame.render_widget(
            Paragraph::new(lines).block(block).scroll((self.scroll, 0)),
            editor,
        );

        let footer_text = self.error.as_deref().map_or_else(
            || " Enter send   Shift+Enter newline   Esc cancel   Ctrl+W delete word ".to_owned(),
            |error| format!(" {error}   (Enter to retry, Esc cancel) "),
        );
        let footer_style = if self.error.is_some() {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().add_modifier(Modifier::DIM)
        };
        frame.render_widget(Paragraph::new(footer_text).style(footer_style), footer);

        if inner_height > 0 {
            let (row, column) = self.textarea.cursor();
            let row = row as u16;
            if row >= self.scroll && row - self.scroll < inner_height {
                let number_width = self.textarea.lines().len().to_string().len() as u16;
                frame.set_cursor_position(Position::new(
                    editor
                        .x
                        .saturating_add(1)
                        .saturating_add(number_width)
                        .saturating_add(1)
                        .saturating_add(column as u16),
                    editor.y.saturating_add(1).saturating_add(row - self.scroll),
                ));
            }
        }
    }

    fn keep_cursor_visible(&mut self, height: u16) {
        let row = self.textarea.cursor().0 as u16;
        if row < self.scroll {
            self.scroll = row;
        } else if height > 0 && row >= self.scroll.saturating_add(height) {
            self.scroll = row.saturating_add(1).saturating_sub(height);
        }
        let max_scroll = (self.textarea.lines().len() as u16).saturating_sub(height);
        self.scroll = self.scroll.min(max_scroll);
    }

    fn render_lines(&self) -> Vec<Line<'_>> {
        let number_width = self.textarea.lines().len().to_string().len();
        let (cursor_row, cursor_column) = self.textarea.cursor();
        self.textarea
            .lines()
            .iter()
            .enumerate()
            .map(|(row, text)| {
                let number = format!("{:>number_width$} ", row + 1);
                let number_style = Style::default().fg(Color::DarkGray);
                if row != cursor_row {
                    Line::from(vec![Span::styled(number, number_style), Span::raw(text)])
                } else {
                    let (before, cursor, after) = cursor_parts(text, cursor_column);
                    Line::from(vec![
                        Span::styled(number, number_style),
                        Span::raw(before),
                        Span::styled(cursor, Style::default().add_modifier(Modifier::REVERSED)),
                        Span::raw(after),
                    ])
                }
            })
            .collect()
    }
}

fn cursor_parts(text: &str, column: usize) -> (&str, String, &str) {
    let byte = text
        .char_indices()
        .nth(column)
        .map_or(text.len(), |(byte, _)| byte);
    let mut chars = text[byte..].chars();
    let cursor = chars
        .next()
        .map_or_else(|| " ".to_owned(), |c| c.to_string());
    let after = byte + text[byte..].chars().next().map_or(0, char::len_utf8);
    (&text[..byte], cursor, &text[after..])
}

fn input_from_key(key: KeyEvent) -> Input {
    let textarea_key = match key.code {
        KeyCode::Char(character) => Key::Char(character),
        KeyCode::F(number) => Key::F(number),
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Enter => Key::Enter,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Tab | KeyCode::BackTab => Key::Tab,
        KeyCode::Delete => Key::Delete,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Esc => Key::Esc,
        _ => Key::Null,
    };
    Input {
        key: textarea_key,
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
        shift: key.modifiers.contains(KeyModifiers::SHIFT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn shift_enter_inserts_a_newline_but_enter_submits() {
        let mut editor = TaskEditor::new("worker-01");
        editor.on_key(key(KeyCode::Char('f'), KeyModifiers::NONE));
        editor.on_key(key(KeyCode::Char('i'), KeyModifiers::NONE));
        editor.on_key(key(KeyCode::Char('r'), KeyModifiers::NONE));
        editor.on_key(key(KeyCode::Char('s'), KeyModifiers::NONE));
        editor.on_key(key(KeyCode::Char('t'), KeyModifiers::NONE));

        assert_eq!(
            editor.on_key(key(KeyCode::Enter, KeyModifiers::SHIFT)),
            TaskEditorAction::Continue
        );

        for character in "second".chars() {
            editor.on_key(key(KeyCode::Char(character), KeyModifiers::NONE));
        }

        assert_eq!(
            editor.on_key(key(KeyCode::Enter, KeyModifiers::NONE)),
            TaskEditorAction::Submit("first\nsecond".to_owned())
        );
    }

    #[test]
    fn standard_editor_keys_move_and_delete_text() {
        let mut editor = TaskEditor::new("worker-01");
        for character in "hello world".chars() {
            editor.on_key(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        editor.on_key(key(KeyCode::Home, KeyModifiers::NONE));
        editor.on_key(key(KeyCode::Delete, KeyModifiers::NONE));
        editor.on_key(key(KeyCode::End, KeyModifiers::NONE));
        editor.on_key(key(KeyCode::Backspace, KeyModifiers::NONE));

        assert_eq!(editor.prompt(), "ello worl");
    }

    #[test]
    fn bracketed_multiline_paste_is_inserted_as_one_edit() {
        let mut editor = TaskEditor::new("worker-01");

        assert_eq!(
            editor.on_event(Event::Paste("first\nsecond".to_owned())),
            TaskEditorAction::Continue
        );
        assert_eq!(editor.prompt(), "first\nsecond");
    }

    #[test]
    fn escape_cancels_without_submitting() {
        let mut editor = TaskEditor::new("worker-01");
        editor.on_event(Event::Paste("do not send".to_owned()));

        assert_eq!(
            editor.on_key(key(KeyCode::Esc, KeyModifiers::NONE)),
            TaskEditorAction::Cancel
        );
    }

    #[test]
    fn whitespace_only_prompt_stays_open_with_an_error() {
        let mut editor = TaskEditor::new("worker-01");
        editor.on_event(Event::Paste(" \n\t".to_owned()));

        assert_eq!(
            editor.on_key(key(KeyCode::Enter, KeyModifiers::NONE)),
            TaskEditorAction::Continue
        );
        assert!(editor.error.is_some());
    }

    #[test]
    fn full_page_render_shows_target_line_numbers_and_prompt() {
        let mut editor = TaskEditor::new("worker-01");
        editor.on_event(Event::Paste("first\nsecond".to_owned()));
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).expect("test terminal");

        terminal
            .draw(|frame| editor.render(frame))
            .expect("editor should render");

        let rendered = terminal.backend().to_string();
        assert!(rendered.contains("Send task to worker-01"), "{rendered}");
        assert!(rendered.contains("1"), "{rendered}");
        assert!(rendered.contains("2"), "{rendered}");
        assert!(rendered.contains("first"), "{rendered}");
        assert!(rendered.contains("Shift+Enter"), "{rendered}");
    }
}
