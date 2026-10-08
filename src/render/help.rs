//! The `?` keybindings overlay: a centered, bordered box listing every binding
//! (label-left, key-right) drawn over a dimmed copy of the view behind it.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// Draw the bindings overlay over whatever is already on `frame`.
pub(crate) fn draw(frame: &mut Frame, bindings: &[(&str, &str)]) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn draws_every_binding_label_and_key() {
        let bindings = [("Move down", "j / ↓"), ("Quit", "q / esc")];
        let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
        term.draw(|frame| draw(frame, &bindings)).unwrap();
        let text = term.backend().to_string();
        assert!(text.contains("Move down"), "{text}");
        assert!(text.contains("j / ↓"), "{text}");
        assert!(text.contains("Quit"), "{text}");
        assert!(text.contains("Keybindings"), "{text}");
    }

    #[test]
    fn a_tiny_terminal_clamps_the_popup_instead_of_panicking() {
        let bindings = [("A very long label", "x")];
        let mut term = Terminal::new(TestBackend::new(6, 3)).unwrap();
        term.draw(|frame| draw(frame, &bindings)).unwrap();
    }
}
