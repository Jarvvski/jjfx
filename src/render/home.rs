//! Home-list rendering: the Attention group headers and the workspace rows,
//! drawn from the read-only [`HomeRow`] projection. No `App` dependency.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use crate::attention::Attention;
use crate::render::graph;
use crate::render::state::RenderState;
use crate::render::style::{
    agent_color, agent_glyph, agent_label, attention_color, behind_color, display_path,
    elapsed_label, forge_spans, pane_border, work_color,
};
use crate::render::view::{HomeRow, RowMarker};
use wsg_core::{WorkerSnapshot, WorkerStatus};

/// Minimum rows (borders included) the inline world pane needs; when half the
/// home body is shorter than this the pane is dropped so the list stays usable.
const MIN_WORLD_PANE_HEIGHT: u16 = 5;

/// One rendered home-list line: a group header or a workspace row.
pub(crate) enum HomeRowKind<'a> {
    Header { attention: Attention, count: usize },
    Workspace(HomeRow<'a>),
}

/// A read-only projection of the home list: the classified rows plus the
/// content the title and inline world pane need.
pub(crate) struct HomeView<'a> {
    pub rows: Vec<HomeRowKind<'a>>,
    pub selected: Option<&'a str>,
    pub idle_collapsed: bool,
    pub tick: u64,
    pub graph: Option<&'a crate::graph::Graph>,
    pub title: String,
}

/// Draw the Attention-grouped list plus its header and footer. The only mutable
/// input is the renderer's own cursor state.
pub(crate) fn draw(
    frame: &mut Frame,
    view: &HomeView<'_>,
    state: &RenderState,
    footer: Paragraph<'_>,
) {
    let [header, body, footer_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .horizontal_margin(2)
    .areas(frame.area());

    frame.render_widget(
        Paragraph::new(Span::styled(
            view.title.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        header,
    );

    // The optional world-graph pane under the list: content-sized, capped at
    // half the body, and dropped entirely when the body is too short to split
    // usefully (the list stays readable - graceful degradation).
    let world_lines = (state.world.is_some() && body.height / 2 >= MIN_WORLD_PANE_HEIGHT)
        .then(|| graph::world_lines(view.graph, view.selected, body.width));
    let world_areas = world_lines.as_ref().map(|lines| {
        let h = (lines.len() as u16).saturating_add(2).min(body.height / 2);
        Layout::vertical([Constraint::Min(0), Constraint::Length(h)]).areas(body)
    });
    let list_area = world_areas.map_or(body, |[list_area, _]| list_area);

    let mut cursor = None;
    let items: Vec<ListItem> = view
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| match row {
            HomeRowKind::Header { attention, count } => {
                header_item(*attention, *count, view.idle_collapsed)
            }
            HomeRowKind::Workspace(row) => {
                let is_selected = view.selected == Some(row.workspace.name.as_str());
                if is_selected {
                    cursor = Some(i);
                }
                workspace_item(row, is_selected, list_area.width, view.tick)
            }
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(cursor);
    frame.render_stateful_widget(List::new(items), list_area, &mut list_state);

    if let (Some([_, world_area]), Some(lines)) = (world_areas, world_lines)
        && let Some(vp) = &state.world
    {
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

    frame.render_widget(footer, footer_area);
}
/// A group-header row: the Attention heading, count, and a fold hint for idle.
pub(crate) fn header_item(att: Attention, count: usize, idle_collapsed: bool) -> ListItem<'static> {
    let mut text = format!("{} ({count})", att.heading());
    if att == Attention::Idle {
        text.push_str(if idle_collapsed {
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
/// and path. The row's axes come from the classified view rather than being
/// re-read, so what the row shows has one source.
pub(crate) fn workspace_item(
    row: &HomeRow<'_>,
    selected: bool,
    width: u16,
    tick: u64,
) -> ListItem<'static> {
    let HomeRow {
        workspace: w,
        attention: att,
        agent,
        work,
        worker,
        behind,
        marker,
    } = *row;
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
        agent_glyph(agent, tick),
        Span::styled(
            format!("{:<11}", agent_label(agent)),
            Style::default().fg(agent_color(agent)),
        ),
    ];
    // While a forge is running, its live pipeline takes the work column;
    // otherwise the work label shows there. A deleting tombstone and a
    // background lift each get their own marker so they remain understandable
    // while selected.
    let deleting = matches!(marker, RowMarker::Deleting);
    match marker {
        RowMarker::Deleting => {
            spans.push(Span::styled(
                format!("{:<16}", "deleting..."),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::DIM),
            ));
        }
        RowMarker::Lifting => {
            spans.push(Span::styled(
                format!("{:<16}", "lifting..."),
                Style::default().fg(Color::Yellow),
            ));
        }
        RowMarker::Queued => {
            spans.push(Span::styled(
                format!("{:<16}", "queued"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::DIM),
            ));
        }
        RowMarker::Forge(progress) => {
            spans.extend(forge_spans(progress));
        }
        RowMarker::None => {
            spans.push(Span::styled(
                format!("{:<16}", work.label()),
                Style::default().fg(work_color(work)),
            ));
        }
    }
    spans.push(Span::styled(
        format!("{behind_label:<5}"),
        Style::default().fg(behind_color(behind)),
    ));
    // The name is boxed (reversed) when selected - a tight highlight instead
    // of a full-width bar; the path trails in dim.
    let name_style = if selected {
        let mut style = Style::default().add_modifier(Modifier::REVERSED);
        if deleting {
            style = style.add_modifier(Modifier::DIM);
        }
        style
    } else if deleting {
        Style::default()
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::DIM)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    let pad = 18usize.saturating_sub(w.name.chars().count()).max(1);
    spans.push(Span::styled(w.name.clone(), name_style));
    spans.push(Span::styled(
        format!("{:pad$}{path}", ""),
        Style::default().fg(Color::DarkGray),
    ));
    let lines = match worker.and_then(|worker| worker_detail_line(worker, width)) {
        Some(detail) => vec![Line::from(spans), detail],
        None => vec![Line::from(spans)],
    };
    ListItem::new(lines)
}

/// The second line for a Worker-backed workspace: its id/alias, status, runtime,
/// ticket, and last activity. Idle unassigned Workers contribute nothing beyond
/// the main row, so this returns `None` for them.
fn worker_detail_line(worker: &WorkerSnapshot, width: u16) -> Option<Line<'static>> {
    let status = if worker.status() == WorkerStatus::Busy && worker.has_dead_process() {
        "stale"
    } else {
        worker.status().as_str()
    };
    let ticket = worker.ticket();

    // An idle, unassigned Worker contributes no information beyond the main
    // workspace row. Keep it there rather than adding a placeholder-only line.
    if worker.status() == WorkerStatus::Idle && ticket.is_none() {
        return None;
    }

    if width < 55 {
        let mut fields = vec![worker.worker_id().to_string(), status.to_owned()];
        if let Some(ticket) = ticket {
            fields.push(format!("ticket:{ticket}"));
        }
        return Some(Line::from(Span::styled(
            format!("  {}", fields.join("  ")),
            Style::default().fg(Color::Cyan),
        )));
    }

    let mut fields = vec![worker.alias().to_owned(), status.to_owned()];
    if let Some(runtime) = worker.agent_runtime() {
        fields.push(runtime.as_str().to_owned());
    }
    if let Some(ticket) = ticket {
        fields.push(format!("ticket:{ticket}"));
    }
    if let Some(activity) = worker
        .last_activity_at()
        .or_else(|| worker.started_at())
        .map(elapsed_label)
    {
        fields.push(activity);
    }

    Some(Line::from(vec![
        Span::styled("  wsg ", Style::default().fg(Color::DarkGray)),
        Span::styled(fields.join("  "), Style::default().fg(Color::Cyan)),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Agent, AgentState};
    use crate::store::Workspace;
    use crate::work::WorkState;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn row<'a>(workspace: &'a Workspace, attention: Attention) -> HomeRow<'a> {
        HomeRow {
            workspace,
            attention,
            agent: Agent {
                state: AgentState::Absent,
                ..Default::default()
            },
            work: WorkState::Clean,
            worker: None,
            behind: 0,
            marker: RowMarker::None,
        }
    }

    #[test]
    fn draws_the_title_group_headers_and_workspace_names() {
        let ws = Workspace {
            name: "feature".to_string(),
            path: None,
        };
        let view = HomeView {
            rows: vec![
                HomeRowKind::Header {
                    attention: Attention::NeedsYou,
                    count: 1,
                },
                HomeRowKind::Workspace(row(&ws, Attention::NeedsYou)),
            ],
            selected: Some("feature"),
            idle_collapsed: false,
            tick: 0,
            graph: None,
            title: "jjfx - 1 workspace(s)".to_string(),
        };
        let state = RenderState::default();
        let mut term = Terminal::new(TestBackend::new(80, 12)).unwrap();
        term.draw(|frame| draw(frame, &view, &state, Paragraph::new("")))
            .unwrap();
        let text = term.backend().to_string();
        assert!(text.contains("jjfx - 1 workspace(s)"), "{text}");
        assert!(text.contains("needs you"), "{text}");
        assert!(text.contains("feature"), "{text}");
    }

    #[test]
    fn a_too_short_body_drops_the_world_pane_but_keeps_the_list() {
        let ws = Workspace {
            name: "feature".to_string(),
            path: None,
        };
        let view = HomeView {
            rows: vec![HomeRowKind::Workspace(row(&ws, Attention::Idle))],
            selected: Some("feature"),
            idle_collapsed: false,
            tick: 0,
            graph: None,
            title: "jjfx".to_string(),
        };
        let state = RenderState {
            world: Some(crate::viewport::Viewport::default()),
            ..RenderState::default()
        };
        // Three rows total leaves no half-body for the pane; it must be dropped.
        let mut term = Terminal::new(TestBackend::new(120, 4)).unwrap();
        term.draw(|frame| draw(frame, &view, &state, Paragraph::new("")))
            .unwrap();
        let text = term.backend().to_string();
        assert!(text.contains("feature"), "{text}");
        assert!(!text.contains(" world "), "{text}");
    }
}
