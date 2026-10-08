//! The full-screen diff detail: a title line, a horizontal split of the
//! changed-file list and the selected file's highlighted diff, and (room
//! permitting) the per-workspace graph strip on the right.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::diff_view::Detail;
use crate::graph;
use crate::render::graph::render_graph_pane;

/// Width of the changed-file list in the detail view (borders included).
const FILES_PANE_WIDTH: u16 = 34;
/// Width of the per-workspace graph strip in the detail view (borders included).
const GRAPH_PANE_WIDTH: u16 = 34;
/// Minimum diff width to keep the strip; below this the strip is dropped so a
/// narrow terminal keeps a readable diff.
const MIN_DIFF_WIDTH: u16 = 40;

/// Draw the diff detail for `detail`, with `graph` feeding the optional strip.
pub(crate) fn draw(frame: &mut Frame, detail: &mut Detail, graph: Option<&graph::Graph>) {
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
                detail.workspace().to_string(),
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
        detail.render_files(frame, files_area);
        detail.render_diff(frame, diff_area);
        render_graph_pane(frame, graph, detail.workspace(), graph_area);
    } else {
        let [files_area, diff_area] =
            Layout::horizontal([Constraint::Length(FILES_PANE_WIDTH), Constraint::Min(0)])
                .areas(body);
        detail.render_files(frame, files_area);
        detail.render_diff(frame, diff_area);
    }

    frame.render_widget(detail.footer(), footer);
}
