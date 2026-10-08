//! Home-list rendering: the Attention group headers and the workspace rows,
//! drawn from the read-only [`HomeRow`] projection. No `App` dependency.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::ListItem;

use crate::attention::Attention;
use crate::render::style::{
    agent_color, agent_glyph, agent_label, attention_color, behind_color, display_path,
    elapsed_label, forge_spans, work_color,
};
use crate::render::view::{HomeRow, RowMarker};
use wsg_core::{WorkerSnapshot, WorkerStatus};

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
