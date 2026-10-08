//! The Worker Pool management view: capacity, Workers and their lifecycle,
//! Dispatch Group progress, previews, and the selected Worker's log. Drawn from
//! a read-only [`PoolView`] over the [`PoolSession`]; no `App` dependency.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::pool_session::PoolSession;
use crate::render::style::dim_line;
use wsg_core::{
    AgentSessionResolution, RunActivity, RunActivityKind, RunConclusion, RunResult, RunUsage,
};

/// Which Pool interaction is focused - the subset of modes the view draws.
pub(crate) enum PoolPane<'a> {
    View,
    DispatchPreview {
        tickets: &'a [String],
        selected: Option<&'a str>,
    },
    ReadyPreview,
    LogDetail {
        worker: &'a str,
    },
}

/// A read-only projection for the Pool view: the session state plus the focused
/// pane and the selected Worker.
pub(crate) struct PoolView<'a> {
    pub session: &'a PoolSession,
    pub pane: PoolPane<'a>,
    pub selected_worker: Option<&'a str>,
}

/// Draw the Pool view body; `footer` is rendered into the bottom strip.
pub(crate) fn draw(frame: &mut Frame, view: &PoolView<'_>, footer: Paragraph<'_>) {
    let [header, body, footer_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .horizontal_margin(2)
    .areas(frame.area());
    frame.render_widget(
        Paragraph::new(Span::styled(
            "Worker Pool  [p management]",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        header,
    );

    let mut lines = Vec::new();
    if let Some(snapshot) = &view.session.worker_pool {
        let capacity = snapshot
            .pool()
            .and_then(|pool| usize::try_from(pool.size()).ok())
            .map_or_else(|| "missing".to_string(), |capacity| capacity.to_string());
        lines.push(Line::from(format!(" capacity: {capacity}")));
        if let Some(runtime) = snapshot.pool().and_then(|pool| pool.agent_runtime()) {
            lines.push(Line::from(format!(" profile: {}", runtime.as_str())));
        }
        if snapshot.workers().is_empty() {
            lines.push(dim_line(" (no Workers)"));
        }
        let selected = view.selected_worker;
        for worker in snapshot.workers() {
            let marker = if selected == Some(worker.worker_id().as_str()) {
                "▸"
            } else {
                "·"
            };
            let line = if body.width < 55 {
                format!(
                    " {marker} {}  {}  runtime: {}  ticket:{}",
                    worker.worker_id(),
                    worker.status().as_str(),
                    worker
                        .agent_runtime()
                        .map_or("-", |runtime| runtime.as_str()),
                    worker.ticket().unwrap_or("-")
                )
            } else {
                format!(
                    " {marker} {}  {:<7} {}  runtime: {}  {}",
                    worker.worker_id(),
                    worker.status().as_str(),
                    worker.alias(),
                    worker
                        .agent_runtime()
                        .map_or("-", |runtime| runtime.as_str()),
                    worker.workspace()
                )
            };
            lines.push(Line::from(line));
        }
        for diagnostic in snapshot.diagnostics() {
            lines.push(Line::from(Span::styled(
                format!(" ! {}", diagnostic.message()),
                Style::default().fg(Color::Yellow),
            )));
        }
        if let Some(progress) = &view.session.group_progress {
            let counts = progress.counts();
            let terminal = if progress.is_terminal() {
                " terminal"
            } else {
                ""
            };
            lines.push(dim_line(&format!(
                " Dispatch Group {}  runtime: {}  waves: {}  done: {}  failed: {}  skipped: {}{}",
                progress.parent(),
                progress.runtime().as_str(),
                progress.maximum_wave(),
                counts.done(),
                counts.failed(),
                counts.skipped(),
                terminal
            )));
            if !progress.ready().is_empty() {
                lines.push(Line::from(format!(
                    "  ready: {}",
                    progress
                        .ready()
                        .iter()
                        .map(wsg_core::TicketId::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            for issue in progress.issues() {
                let blockers = if issue.blockers().is_empty() {
                    String::new()
                } else {
                    format!(
                        "  blocked by {}",
                        issue
                            .blockers()
                            .iter()
                            .map(wsg_core::TicketId::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                let retry = if issue.retries() > 0 {
                    format!("  retry {}", issue.retries())
                } else {
                    String::new()
                };
                lines.push(Line::from(format!(
                    "  wave {}  {:<10} {}  {} -> {}{}{}",
                    issue.wave(),
                    issue.status().as_str(),
                    issue.ticket(),
                    issue.title(),
                    issue
                        .worker()
                        .map_or("unassigned", wsg_core::WorkerId::as_str),
                    retry,
                    blockers
                )));
            }
        }
        match &view.pane {
            PoolPane::DispatchPreview { tickets, selected } => {
                lines.push(dim_line(" Dispatch preview:"));
                for ticket in *tickets {
                    let target = if tickets.len() == 1 {
                        selected.unwrap_or("no Worker")
                    } else {
                        "first idle Worker"
                    };
                    lines.push(Line::from(format!("  ? {ticket} -> {target}")));
                }
            }
            PoolPane::ReadyPreview => {
                if let Some(ready) = &view.session.ready_tickets {
                    lines.push(dim_line(" Ready Ticket preview:"));
                    for ticket in ready.tickets() {
                        lines.push(Line::from(format!(
                            "  ? {}  {}",
                            ticket.id(),
                            ticket.title()
                        )));
                    }
                    for diagnostic in ready.diagnostics() {
                        lines.push(Line::from(Span::styled(
                            format!(" ! {diagnostic}"),
                            Style::default().fg(Color::Yellow),
                        )));
                    }
                }
            }
            _ => {}
        }
        if let Some(session) = &view.session.worker_session {
            let session_text = match session.session() {
                AgentSessionResolution::Resumed { session_id } => {
                    format!("resumed session {session_id}")
                }
                AgentSessionResolution::Fresh { reason } => {
                    format!("fresh session ({reason})")
                }
            };
            lines.push(dim_line(&format!(
                " {} {}  runtime: {}  PID {}",
                session.action().as_str(),
                session.worker(),
                session.runtime().as_str(),
                session.pid()
            )));
            lines.push(Line::from(format!("  {session_text}")));
        }
        if let Some(result) = &view.session.worker_command_result {
            lines.push(dim_line(&format!(" Worker action: {}", result.notice())));
        }
        if let Some(result) = &view.session.dispatch_result {
            lines.push(dim_line(&format!(
                " Dispatch outcomes (runtime: {}):",
                result.runtime().as_str()
            )));
            for outcome in result.outcomes() {
                let line = if outcome.succeeded() {
                    format!(
                        "  ✓ {}  {} -> {} (PID {})",
                        outcome.ticket(),
                        outcome.title(),
                        outcome.worker().unwrap_or("?"),
                        outcome.pid().unwrap_or_default()
                    )
                } else {
                    format!(
                        "  ✗ {} [{}] {}",
                        outcome.ticket(),
                        outcome.phase().unwrap_or("unknown"),
                        outcome.detail().unwrap_or("Dispatch failed")
                    )
                };
                lines.push(Line::from(line));
            }
        }
        if let PoolPane::LogDetail { worker } = &view.pane {
            lines.push(dim_line(&format!(" Worker log: {worker}")));
            if let Some(error) = &view.session.worker_log_error {
                lines.push(Line::from(Span::styled(
                    format!(" ! {error}"),
                    Style::default().fg(Color::Yellow),
                )));
            } else if let Some(snapshot) = &view.session.worker_log {
                lines.push(Line::from(format!(
                    " runtime: {}  worker: {}",
                    snapshot.runtime().as_str(),
                    snapshot.worker()
                )));
                match snapshot.activity() {
                    Some(activity) => lines.push(Line::from(format!(
                        " activity: {}",
                        worker_activity_line(activity)
                    ))),
                    None => lines.push(dim_line(" no recognized activity yet")),
                }
                if let Some(result) = snapshot.result() {
                    lines.push(Line::from(format!(
                        " result: {}",
                        worker_result_line(result)
                    )));
                }
            } else {
                lines.push(dim_line(" loading latest activity..."));
            }
        }
    } else {
        lines.push(dim_line(" loading Worker Pool..."));
    }
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" pool ")),
        body,
    );
    frame.render_widget(footer, footer_area);
}

fn worker_activity_line(activity: &RunActivity) -> String {
    let detail = match activity.kind() {
        RunActivityKind::SessionStarted => "session started".to_owned(),
        RunActivityKind::Message { text } => text.clone(),
        RunActivityKind::Reasoning { text } => format!("reasoning: {text}"),
        RunActivityKind::Warning { message } => format!("warning: {message}"),
        RunActivityKind::FileChanges { paths } => format!("files: {}", paths.join(", ")),
        RunActivityKind::Plan { completed, total } => format!("plan: {completed}/{total}"),
        RunActivityKind::Tool {
            name,
            detail,
            status,
        } => format!(
            "tool {name} {status:?}{}",
            detail
                .as_deref()
                .map_or(String::new(), |value| format!(" {value}"))
        ),
        RunActivityKind::Collaboration(event) => format!("collaboration: {:?}", event),
    };
    match activity.usage() {
        Some(usage) => format!("{detail} [{}]", worker_usage_line(usage)),
        None => detail,
    }
}

fn worker_usage_line(usage: &RunUsage) -> String {
    format!(
        "input {} cached {} output {} reasoning {}",
        usage.input_tokens(),
        usage.cached_input_tokens(),
        usage.output_tokens(),
        usage.reasoning_output_tokens()
    )
}

fn worker_result_line(result: &RunResult) -> String {
    match result.conclusion() {
        RunConclusion::Succeeded => "succeeded".to_owned(),
        RunConclusion::Failed { message } => format!("failed: {message}"),
    }
}
