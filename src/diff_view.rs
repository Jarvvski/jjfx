//! The progressive-disclosure diff viewer for one workspace (ADR 0008): a
//! changed-file list on the left, the selected file's syntect-highlighted diff
//! on the right. State, key handling, and rendering live together here so the
//! viewer is one testable unit; `App` holds a [`Detail`] and forwards keys to it.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::app::pane_border;
use crate::diff::{self, FileDiff};
use crate::viewport::Viewport;

/// Width of a file's +/- magnitude bar, in cells.
const BAR_W: usize = 8;

/// Which pane owns the keyboard: the changed-file list or the scrolling diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailFocus {
    Files,
    Diff,
}

/// The progressive-disclosure diff view for one workspace: a changed-file list
/// with +/- magnitude bars on the left, the selected file's syntect-highlighted
/// diff on the right. The diff is read asynchronously, so it opens `loading`
/// until the diff snapshot lands via [`Detail::loaded`].
pub struct Detail {
    ws: String,
    loading: bool,
    files: Vec<FileDiff>,
    focus: DetailFocus,
    /// Cursor into the *filtered* file list.
    selected: usize,
    /// Fuzzy filter typed against the file paths.
    filter: String,
    /// Vertical scroll of the diff pane. `total` tracks the selected file's line
    /// count; `height` is refreshed at render.
    viewport: Viewport,
    /// Lazy highlighter for the selected file, rebuilt when the selection or
    /// filter changes. It highlights only as far down as the viewport has needed,
    /// so navigating between files never highlights a whole large diff up front.
    /// Boxed - its syntect state is large and would bloat the `Mode` enum inline.
    hl: Option<Box<diff::FileHighlighter>>,
}

impl Detail {
    /// Open the viewer for `ws` in its loading state, before the diff arrives.
    pub fn loading(ws: String) -> Self {
        Detail {
            ws,
            loading: true,
            files: Vec::new(),
            focus: DetailFocus::Files,
            selected: 0,
            filter: String::new(),
            viewport: Viewport::default(),
            hl: None,
        }
    }

    /// The workspace this view is showing - the join key for a late diff snapshot.
    pub fn workspace(&self) -> &str {
        &self.ws
    }

    /// Fold a freshly-read diff into the view and select the first file.
    pub fn loaded(&mut self, files: Vec<FileDiff>) {
        self.files = files;
        self.loading = false;
        self.resync();
    }

    /// Indices into `files` whose path matches the current fuzzy filter, in diff
    /// order.
    fn filtered(&self) -> Vec<usize> {
        self.files
            .iter()
            .enumerate()
            .filter(|(_, f)| diff::fuzzy_match(&self.filter, &f.path))
            .map(|(i, _)| i)
            .collect()
    }

    /// The file under the cursor in the filtered list, if any.
    fn current(&self) -> Option<&FileDiff> {
        self.filtered().get(self.selected).map(|&i| &self.files[i])
    }

    /// Clamp the cursor into the (possibly narrowed) filtered range, reset the
    /// scroll, and rehighlight the newly-selected file. Called after any change to
    /// the selection or filter, never on a plain scroll.
    fn resync(&mut self) {
        let len = self.filtered().len();
        if self.selected >= len {
            self.selected = len.saturating_sub(1);
        }
        // Reset to the top and retarget the viewport at the newly-selected file's
        // line count, so scrolling clamps correctly even before the next render.
        let total = self.current().map(|f| f.lines.len()).unwrap_or(0) as u16;
        self.viewport.jump_top();
        self.viewport.set_total(total);
        // A fresh lazy highlighter for the new file; it highlights nothing until
        // the diff pane renders and asks for the visible window.
        self.hl = self
            .current()
            .map(|f| Box::new(diff::FileHighlighter::new(f)));
    }

    /// Handle one key. Depth is `list → files → diff`: `→` goes deeper, `←`
    /// shallower, `esc` jumps straight back to the list. In the files pane typing
    /// fuzzy-filters and the arrows move the cursor; in the diff pane `j/k` and
    /// `PgUp/PgDn` scroll. `tab` toggles the two panes. Returns `true` when the
    /// view should close (the caller owns the mode transition).
    pub fn on_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match self.focus {
            DetailFocus::Files => match key.code {
                KeyCode::Esc | KeyCode::Left => return true,
                KeyCode::Up => {
                    self.selected = self.selected.saturating_sub(1);
                    self.resync();
                }
                KeyCode::Down => {
                    self.selected += 1;
                    self.resync();
                }
                KeyCode::Right | KeyCode::Tab => {
                    if !self.filtered().is_empty() {
                        self.focus = DetailFocus::Diff;
                    }
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.selected = 0;
                    self.resync();
                }
                // Any printable char (not a Ctrl chord) extends the filter.
                KeyCode::Char(c) if !ctrl => {
                    self.filter.push(c);
                    self.selected = 0;
                    self.resync();
                }
                _ => {}
            },
            DetailFocus::Diff => match key.code {
                KeyCode::Esc => return true,
                KeyCode::Left | KeyCode::Char('h') | KeyCode::Tab | KeyCode::BackTab => {
                    self.focus = DetailFocus::Files;
                }
                KeyCode::Char('j') | KeyCode::Down => self.viewport.line_down(),
                KeyCode::Char('k') | KeyCode::Up => self.viewport.line_up(),
                KeyCode::Char('d') if ctrl => self.viewport.half_page_down(),
                KeyCode::Char('u') if ctrl => self.viewport.half_page_up(),
                KeyCode::PageDown => self.viewport.page_down(),
                KeyCode::PageUp => self.viewport.page_up(),
                KeyCode::Char('g') => self.viewport.jump_top(),
                KeyCode::Char('G') => self.viewport.jump_bottom(),
                _ => {}
            },
        }
        false
    }

    /// The changed-file list: a cursor bullet, a +/- magnitude bar, and the elided
    /// path. Shows loading / empty states in place of the list.
    pub fn render_files(&self, frame: &mut Frame, area: Rect) {
        let focused = self.focus == DetailFocus::Files;
        let title = if self.filter.is_empty() {
            " files ".to_string()
        } else {
            format!(" files  /{} ", self.filter)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(pane_border(focused))
            .title(title);

        if self.loading {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    " loading…",
                    Style::default().add_modifier(Modifier::DIM),
                ))
                .block(block),
                area,
            );
            return;
        }

        let filtered = self.filtered();
        if filtered.is_empty() {
            let msg = if self.files.is_empty() {
                " no changes from trunk"
            } else {
                " no files match"
            };
            frame.render_widget(
                Paragraph::new(Span::styled(
                    msg,
                    Style::default().add_modifier(Modifier::DIM),
                ))
                .block(block),
                area,
            );
            return;
        }

        let max_total = self
            .files
            .iter()
            .map(|f| f.added + f.removed)
            .max()
            .unwrap_or(0)
            .max(1);
        // Inner width less the border (2), the bullet (2), the bar and its gap.
        let name_w = (area.width as usize)
            .saturating_sub(2 + 2 + BAR_W + 1)
            .max(4);

        let items: Vec<ListItem> = filtered
            .iter()
            .enumerate()
            .map(|(row, &fi)| {
                let f = &self.files[fi];
                let is_sel = row == self.selected;
                let bullet = if is_sel {
                    Span::styled("▸ ", Style::default().fg(Color::White))
                } else {
                    Span::raw("  ")
                };
                let mut spans = vec![bullet];
                spans.extend(ratio_bar(f.added, f.removed, max_total));
                spans.push(Span::raw(" "));
                let name_style = if is_sel && focused {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else if is_sel {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                spans.push(Span::styled(elide_left(&f.path, name_w), name_style));
                ListItem::new(Line::from(spans))
            })
            .collect();

        let mut state = ListState::default();
        state.select(Some(self.selected));
        frame.render_stateful_widget(List::new(items).block(block), area, &mut state);
    }

    /// The selected file's diff. Highlighting is advanced lazily to the bottom of
    /// the viewport and only the visible slice is cloned, so both switching files
    /// and scrolling a large diff stay bounded by the viewport, not the diff size
    /// (AC: large diffs must not block the render loop).
    pub fn render_diff(&mut self, frame: &mut Frame, area: Rect) {
        let focused = self.focus == DetailFocus::Diff;
        // The file index up front, so files and the highlighter can be borrowed
        // disjointly below.
        let fi = self.filtered().get(self.selected).copied();
        let title = match fi {
            Some(i) => format!(" {} ", self.files[i].path),
            None => " diff ".to_string(),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(pane_border(focused))
            .title(title);

        // The visible height feeds page/scroll math and clamping on the next key.
        let inner_h = area.height.saturating_sub(2) as usize;

        let Some(fi) = fi else {
            // Nothing to show: keep the viewport height current (nothing to scroll).
            self.viewport.resize(inner_h as u16, 0);
            let msg = if self.loading { " loading…" } else { "" };
            frame.render_widget(
                Paragraph::new(Span::styled(
                    msg,
                    Style::default().add_modifier(Modifier::DIM),
                ))
                .block(block),
                area,
            );
            return;
        };

        // Record the geometry and clamp scroll to the file's line count (known
        // without highlighting).
        let total = self.files[fi].lines.len();
        self.viewport.resize(inner_h as u16, total as u16);
        let start = (self.viewport.scroll() as usize).min(total);
        let end = (start + inner_h).min(total);

        let Some(hl) = self.hl.as_mut() else {
            frame.render_widget(Paragraph::new("").block(block), area);
            return;
        };
        // Highlight only as far as the viewport bottom, extending a chunk at a time.
        hl.ensure(&self.files[fi], end);
        let ready = hl.ready();
        let hi_end = end.min(ready.len());
        let visible: Vec<Line> = ready[start.min(hi_end)..hi_end].to_vec();
        frame.render_widget(Paragraph::new(visible).block(block), area);
    }

    /// The footer hint for the focused pane.
    pub fn footer(&self) -> Paragraph<'static> {
        let hint = match self.focus {
            DetailFocus::Files => " type filter · ↑/↓ file · →/tab diff · esc back ",
            DetailFocus::Diff => " j/k scroll · PgUp/PgDn page · ←/tab files · esc back ",
        };
        Paragraph::new(Span::styled(
            hint,
            Style::default().add_modifier(Modifier::DIM),
        ))
    }
}

/// A fixed-width +/- magnitude bar: green cells for insertions, red for
/// deletions (scaled so the busiest file fills the bar), dim dots for the rest.
fn ratio_bar(added: u32, removed: u32, max_total: u32) -> Vec<Span<'static>> {
    let total = added + removed;
    let filled = if total == 0 {
        0
    } else {
        (((total as f64 / max_total as f64) * BAR_W as f64).round() as usize).clamp(1, BAR_W)
    };
    let greens = if total == 0 {
        0
    } else {
        (((added as f64 / total as f64) * filled as f64).round() as usize).min(filled)
    };
    let reds = filled - greens;
    let empty = BAR_W - filled;
    vec![
        Span::styled("█".repeat(greens), Style::default().fg(Color::Green)),
        Span::styled("█".repeat(reds), Style::default().fg(Color::Red)),
        Span::styled("·".repeat(empty), Style::default().fg(Color::DarkGray)),
    ]
}

/// Truncate a path to `max` columns, keeping the tail (filename) with a leading
/// ellipsis when it overflows.
fn elide_left(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let tail: String = s.chars().skip(len - keep).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{DiffLine, LineKind};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    /// Two changed files with distinct magnitudes; the first has three diff lines.
    fn sample_files() -> Vec<FileDiff> {
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

    #[test]
    fn loading_starts_empty_and_waiting() {
        let d = Detail::loading("feat".to_string());
        assert_eq!(d.workspace(), "feat");
        assert!(d.loading);
        assert!(d.files.is_empty());
    }

    #[test]
    fn loaded_populates_and_selects_the_first_file() {
        let mut d = Detail::loading("feat".to_string());
        d.loaded(sample_files());
        assert!(!d.loading);
        assert_eq!(d.files.len(), 2);
        assert_eq!(d.selected, 0);
        assert_eq!(d.current().unwrap().path, "src/app.rs");
        // A highlighter is armed for the selection (lazy - it highlights on the
        // first render, not here).
        assert!(d.hl.is_some());
    }

    #[test]
    fn typing_fuzzy_filters_the_file_list_and_clamps_selection() {
        let mut d = Detail::loading("feat".to_string());
        d.loaded(sample_files());
        // Move to the second file, then filter to only the first.
        d.on_key(key(KeyCode::Down));
        for c in ['a', 'p', 'p'] {
            d.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(d.filter, "app");
        assert_eq!(d.filtered().len(), 1);
        // Selection clamped back into the narrowed range.
        assert_eq!(d.selected, 0);
        assert_eq!(d.current().unwrap().path, "src/app.rs");
        // Backspacing the filter widens it again.
        d.on_key(key(KeyCode::Backspace));
        d.on_key(key(KeyCode::Backspace));
        d.on_key(key(KeyCode::Backspace));
        assert_eq!(d.filtered().len(), 2);
    }

    #[test]
    fn focus_toggles_between_panes_and_the_diff_scrolls() {
        let mut d = Detail::loading("feat".to_string());
        d.loaded(sample_files());
        // Give the diff pane a viewport (height 1 over the 3-line file) so scroll
        // clamps like a real render.
        d.viewport.resize(1, 3);
        // → moves focus into the diff pane; the view stays open.
        assert!(!d.on_key(key(KeyCode::Right)));
        assert_eq!(d.focus, DetailFocus::Diff);
        // j scrolls down; k scrolls back to the top (saturating at 0).
        d.on_key(key(KeyCode::Char('j')));
        assert_eq!(d.viewport.scroll(), 1);
        d.on_key(key(KeyCode::Char('k')));
        d.on_key(key(KeyCode::Char('k')));
        // ← returns focus to the file list (does not exit the view).
        assert!(!d.on_key(key(KeyCode::Left)));
        assert_eq!(d.focus, DetailFocus::Files);
        assert_eq!(d.viewport.scroll(), 0);
    }

    #[test]
    fn esc_and_left_from_the_files_pane_signal_exit() {
        let mut d = Detail::loading("feat".to_string());
        d.loaded(sample_files());
        assert!(
            d.on_key(key(KeyCode::Left)),
            "left from files closes the view"
        );

        let mut d = Detail::loading("feat".to_string());
        d.loaded(sample_files());
        assert!(
            d.on_key(key(KeyCode::Esc)),
            "esc from files closes the view"
        );
    }
}
