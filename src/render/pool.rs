//! The Worker Pool management view: capacity, Workers and their lifecycle,
//! Dispatch Group progress, previews, and the selected Worker's log. Drawn from
//! a read-only [`PoolView`] over the [`PoolSession`]; no `App` dependency.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::engine::{
    AgentSessionResolution, PoolCostSnapshot, RunActivity, RunActivityKind, RunConclusion,
    RunResult, RunUsage, StuckWindow, WorkerPoolSnapshot, WorkerStatus,
};
use crate::pool_session::PoolSession;
use crate::render::style::dim_line;
use jiff::Timestamp;

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

/// A read-only projection for the Worker Pool view: the session state plus the
/// focused pane, the selected Worker, and the clock and stuck-Run window the
/// watchdog is judged against.
pub(crate) struct PoolView<'a> {
    pub session: &'a PoolSession,
    pub pane: PoolPane<'a>,
    pub selected_worker: Option<&'a str>,
    pub watchdog: StuckWindow,
    pub now: Timestamp,
}

/// Draw the Worker Pool view body; `footer` is rendered into the bottom strip.
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
        lines.push(match snapshot.cost() {
            Some(cost) => Line::from(format!(" {}", pool_cost_line(cost))),
            None => dim_line(" spend: record store unavailable"),
        });
        if snapshot.workers().is_empty() {
            lines.push(dim_line(" (no Workers)"));
        }
        if !snapshot.waiting().is_empty() {
            lines.push(Line::from(format!(
                " waiting: {}",
                snapshot.waiting().join(", ")
            )));
        }
        let selected = view.selected_worker;
        for worker in snapshot.workers() {
            let marker = if selected == Some(worker.worker_id().as_str()) {
                "▸"
            } else {
                "·"
            };
            // An open Workspace is not eligible capacity, so surface it beside
            // the capacity status rather than replacing it.
            let open = if snapshot.is_occupied(worker.worker_id().as_str()) {
                "  [open]"
            } else {
                ""
            };
            // A stuck Run is still `busy` in the compatible document: the
            // watchdog's verdict is a view of it, surfaced alongside rather than
            // in place of the persisted status.
            let stuck = if worker.health(view.now, view.watchdog).is_stuck() {
                "  [stuck]"
            } else {
                ""
            };
            let line = if body.width < 55 {
                format!(
                    " {marker} {}  {}  runtime: {}  ticket:{}{stuck}{open}",
                    worker.worker_id(),
                    worker.status().as_str(),
                    worker
                        .agent_runtime()
                        .map_or("-", |runtime| runtime.as_str()),
                    worker.ticket().unwrap_or("-")
                )
            } else {
                format!(
                    " {marker} {}  {:<7} {}  runtime: {}  {}{stuck}{open}{spend}",
                    worker.worker_id(),
                    worker.status().as_str(),
                    worker.alias(),
                    worker
                        .agent_runtime()
                        .map_or("-", |runtime| runtime.as_str()),
                    worker.workspace(),
                    spend = worker_spend(snapshot, worker.worker_id().as_str())
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
            let blocked = if counts.blocked() > 0 {
                format!("  blocked: {}", counts.blocked())
            } else {
                String::new()
            };
            lines.push(dim_line(&format!(
                " Dispatch Group {}  runtime: {}  waves: {}  done: {}  failed: {}  skipped: {}{}{}",
                progress.parent(),
                progress.runtime().as_str(),
                progress.maximum_wave(),
                counts.done(),
                counts.failed(),
                counts.skipped(),
                blocked,
                terminal
            )));
            if !progress.ready().is_empty() {
                lines.push(Line::from(format!(
                    "  ready: {}",
                    progress
                        .ready()
                        .iter()
                        .map(crate::engine::TicketId::as_str)
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
                            .map(crate::engine::TicketId::as_str)
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
                        .map_or("unassigned", crate::engine::WorkerId::as_str),
                    retry,
                    blockers
                )));
            }
        }
        match &view.pane {
            PoolPane::DispatchPreview { tickets, selected } => {
                lines.push(dim_line(" Dispatch preview:"));
                for ticket in *tickets {
                    let target = match (tickets.len(), selected) {
                        (1, Some(worker)) if snapshot.is_occupied(worker) => {
                            format!("{worker} (open - not eligible)")
                        }
                        (1, Some(worker)) => (*worker).to_owned(),
                        (1, None) => first_eligible_worker(snapshot)
                            .map_or_else(|| "no eligible Worker".to_owned(), ToOwned::to_owned),
                        _ => "first eligible idle Worker".to_owned(),
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
            let delivery = session.pid().map_or_else(
                || "delivered to its session".to_owned(),
                |pid| format!("PID {pid}"),
            );
            lines.push(dim_line(&format!(
                " {} {}  runtime: {}  {}",
                session.action().as_str(),
                session.worker(),
                session.runtime().as_str(),
                delivery
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
                } else if outcome.is_waiting() {
                    format!(
                        "  ⏸ {}  {}",
                        outcome.ticket(),
                        outcome.detail().unwrap_or("held by the dispatch cap")
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

/// The first Worker Dispatch would select: idle and not open in a terminal.
fn first_eligible_worker(snapshot: &WorkerPoolSnapshot) -> Option<&str> {
    snapshot
        .workers()
        .iter()
        .find(|worker| {
            worker.status() == WorkerStatus::Idle
                && !snapshot.is_occupied(worker.worker_id().as_str())
        })
        .map(|worker| worker.worker_id().as_str())
}

/// The pool-level cost/usage line: the windows the view shows, plus the Run
/// count and how many Runs reported no usable cost.
fn pool_cost_line(cost: &PoolCostSnapshot) -> String {
    let total = cost.total();
    let mut line = format!(
        " spend: {} today, {} (7d), {} total  runs: {}  tokens: {}",
        cost.today().cost_display(),
        cost.week().cost_display(),
        total.cost_display(),
        total.runs(),
        total.tokens_display()
    );
    if total.unpriced_runs() > 0 {
        line.push_str(&format!(
            "  ({} Run(s) without a reported cost)",
            total.unpriced_runs()
        ));
    }
    line
}

/// One Worker's recorded spend, or nothing when it recorded no Run.
fn worker_spend(snapshot: &WorkerPoolSnapshot, worker: &str) -> String {
    snapshot
        .cost()
        .and_then(|cost| cost.worker(worker))
        .map_or_else(String::new, |cost| {
            format!("  spend: {}", cost.cost_display())
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Repository, WorkerId};
    use crate::pool_session::PoolSession;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// A one-Worker Pool snapshot, optionally recorded as open in a terminal.
    fn snapshot(occupied: bool) -> WorkerPoolSnapshot {
        let temp = tempfile::tempdir().expect("temp repository");
        let pool = temp.path().join(".jj/pools/default");
        std::fs::create_dir_all(&pool).expect("pool directory");
        std::fs::write(
            temp.path().join(".jj/pools/default/pool.json"),
            br#"{"size":1,"gh_repo":"Jarvvski/jjfx","workers":["worker-01"],"created_at":"2026-07-27T10:00:00Z","names":{}}"#,
        )
        .expect("pool state");
        std::fs::write(
            pool.join("worker-01.json"),
            br#"{"status":"idle","agent":"claude","ticket":null,"pid":null,"started_at":null,"completed_at":null,"log_file":null,"branch_name":null,"exit_code":null,"error":null}"#,
        )
        .expect("worker state");
        let repository = Repository::open(temp.path()).expect("repository");
        if occupied {
            repository
                .state_store()
                .in_use()
                .mark(&WorkerId::parse("worker-01").expect("worker id"))
                .expect("mark open");
        }
        repository.read_worker_pool_snapshot()
    }

    /// A one-Worker Pool snapshot whose Worker is busy with a Run that started at
    /// `started_at`, optionally with a Run log at `log_file`.
    fn snapshot_busy(started_at: &str, log_file: Option<&std::path::Path>) -> WorkerPoolSnapshot {
        let temp = tempfile::tempdir().expect("temp repository");
        let pool = temp.path().join(".jj/pools/default");
        std::fs::create_dir_all(&pool).expect("pool directory");
        std::fs::write(
            temp.path().join(".jj/pools/default/pool.json"),
            br#"{"size":1,"gh_repo":"Jarvvski/jjfx","workers":["worker-01"],"created_at":"2026-07-27T10:00:00Z","names":{}}"#,
        )
        .expect("pool state");
        let log = log_file.map_or_else(String::new, |path| {
            format!(", \"log_file\":{:?}", path.to_string_lossy())
        });
        std::fs::write(
            pool.join("worker-01.json"),
            format!(
                r#"{{"status":"busy","agent":"claude","ticket":"ENG-1","pid":null,"started_at":"{started_at}","completed_at":null{log},"branch_name":null,"exit_code":null,"error":null}}"#
            ),
        )
        .expect("worker state");
        let repository = Repository::open(temp.path()).expect("repository");
        repository.read_worker_pool_snapshot()
    }

    fn render(session: &PoolSession, pane: PoolPane<'_>, selected: Option<&str>) -> String {
        render_at(
            session,
            pane,
            selected,
            Timestamp::now(),
            StuckWindow::DEFAULT,
        )
    }

    /// Render with an explicit watchdog clock and window, for the stuck cases.
    fn render_at(
        session: &PoolSession,
        pane: PoolPane<'_>,
        selected: Option<&str>,
        now: Timestamp,
        watchdog: StuckWindow,
    ) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).expect("test terminal");
        let view = PoolView {
            session,
            pane,
            selected_worker: selected,
            watchdog,
            now,
        };
        terminal
            .draw(|frame| draw(frame, &view, Paragraph::new("")))
            .expect("draw");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn the_worker_list_marks_an_open_workspace() {
        let session = PoolSession {
            worker_pool: Some(snapshot(true)),
            ..PoolSession::default()
        };
        let text = render(&session, PoolPane::View, Some("worker-01"));
        assert!(text.contains("[open]"), "expected an open marker: {text}");
    }

    #[test]
    fn the_worker_list_marks_a_stuck_run_beside_its_busy_status() {
        let session = PoolSession {
            worker_pool: Some(snapshot_busy("2026-01-01T00:00:00Z", None)),
            ..PoolSession::default()
        };
        let now: Timestamp = "2026-01-01T01:00:00Z".parse().expect("a timestamp");
        let text = render_at(
            &session,
            PoolPane::View,
            Some("worker-01"),
            now,
            StuckWindow::DEFAULT,
        );
        assert!(text.contains("[stuck]"), "expected a stuck marker: {text}");
        // The persisted status is still shown: stuck is a view of `busy`.
        assert!(text.contains("busy"), "expected the busy status: {text}");

        // Inside the window it is not stuck, and with the watchdog off it never
        // is: the marker is the only thing the window changes.
        let text = render_at(
            &session,
            PoolPane::View,
            Some("worker-01"),
            "2026-01-01T00:05:00Z".parse().expect("a timestamp"),
            StuckWindow::DEFAULT,
        );
        assert!(!text.contains("[stuck]"), "still inside the window: {text}");
        let text = render_at(
            &session,
            PoolPane::View,
            Some("worker-01"),
            now,
            StuckWindow::DISABLED,
        );
        assert!(!text.contains("[stuck]"), "watchdog disabled: {text}");
    }

    /// A one-Worker Pool snapshot whose Worker has one recorded Run.
    fn snapshot_with_recorded_cost() -> WorkerPoolSnapshot {
        let temp = tempfile::tempdir().expect("temp repository");
        let pool = temp.path().join(".jj/pools/default");
        std::fs::create_dir_all(&pool).expect("pool directory");
        std::fs::write(
            temp.path().join(".jj/pools/default/pool.json"),
            br#"{"size":1,"gh_repo":"Jarvvski/jjfx","workers":["worker-01"],"created_at":"2026-07-27T10:00:00Z","names":{}}"#,
        )
        .expect("pool state");
        std::fs::write(
            pool.join("worker-01.json"),
            br#"{"status":"done","agent":"claude","ticket":"ENG-1","pid":null,"started_at":null,"completed_at":"2026-07-27T10:05:00Z","log_file":null,"branch_name":null,"exit_code":0,"error":null}"#,
        )
        .expect("worker state");
        let repository = Repository::open(temp.path()).expect("repository");
        repository
            .state_store()
            .runs()
            .append(&crate::engine::RunRecord {
                run_id: "worker-01@1".to_owned(),
                worker: WorkerId::parse("worker-01").expect("worker id"),
                ticket: Some("ENG-1".to_owned()),
                workspace: None,
                branch: None,
                runtime: Some("claude".to_owned()),
                provider: None,
                model: None,
                cost_micro_usd: Some(250_000),
                usage: Some(crate::engine::RunRecordUsage {
                    input_tokens: 1_200,
                    cached_input_tokens: 0,
                    cache_write_input_tokens: 0,
                    output_tokens: 340,
                    reasoning_output_tokens: 0,
                }),
                duration_ms: None,
                turns: None,
                conclusion: crate::engine::RunRecordConclusion::Succeeded,
                error: None,
                started_at: None,
                completed_at: Some(crate::engine::WireTimestamp::new("2026-07-27T10:05:00Z")),
            })
            .expect("append record");
        repository.read_worker_pool_snapshot()
    }

    #[test]
    fn the_pool_view_shows_aggregate_spend_usage_and_per_worker_spend() {
        let session = PoolSession {
            worker_pool: Some(snapshot_with_recorded_cost()),
            ..PoolSession::default()
        };
        let text = render(&session, PoolPane::View, Some("worker-01"));
        assert!(text.contains("spend: $0.2500"), "pool spend: {text}");
        assert!(text.contains("total"), "pool total: {text}");
        assert!(text.contains("tokens: in 1.2k out 340"), "usage: {text}");
        assert!(text.contains("runs: 1"), "run count: {text}");
        assert!(text.contains("spend: $0.2500"), "per-worker spend: {text}");
    }

    #[test]
    fn the_pool_view_shows_a_zero_spend_when_no_runs_are_recorded() {
        let session = PoolSession {
            worker_pool: Some(snapshot(false)),
            ..PoolSession::default()
        };
        let text = render(&session, PoolPane::View, Some("worker-01"));
        assert!(text.contains("spend: $0.0000"), "{text}");
        assert!(text.contains("runs: 0"), "{text}");
    }

    #[test]
    fn the_dispatch_preview_flags_an_open_target() {
        let session = PoolSession {
            worker_pool: Some(snapshot(true)),
            ..PoolSession::default()
        };
        let tickets = ["ENG-1".to_owned()];
        let text = render(
            &session,
            PoolPane::DispatchPreview {
                tickets: &tickets,
                selected: Some("worker-01"),
            },
            Some("worker-01"),
        );
        assert!(
            text.contains("(open - not eligible)"),
            "expected the preview to flag the open Worker: {text}"
        );
    }

    #[test]
    fn the_dispatch_preview_names_the_first_eligible_worker() {
        let session = PoolSession {
            worker_pool: Some(snapshot(false)),
            ..PoolSession::default()
        };
        let tickets = ["ENG-1".to_owned()];
        let text = render(
            &session,
            PoolPane::DispatchPreview {
                tickets: &tickets,
                selected: None,
            },
            None,
        );
        assert!(
            text.contains("ENG-1 -> worker-01"),
            "expected the eligible Worker in the preview: {text}"
        );
    }
}
