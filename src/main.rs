//! jjfx - keyboard-driven mission-control for parallel Claude Code agents, each
//! in its own Jujutsu workspace. It loads the authoritative workspace store,
//! mirrors `.jj/ws-cache`, event-sources each workspace's agent lifecycle from
//! Claude Code hooks, and renders an Attention-first list.

mod agent;
mod app;
mod attention;
mod cache;
mod cmd;
mod diff;
mod events;
mod forge;
mod hooks;
mod jj;
mod repo;
mod store;
mod terminal;
mod tui;
mod watch;
mod work;

use std::io::Write;

use anyhow::Context;
use ratatui::crossterm::event;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::app::{App, Msg};
use crate::store::Store;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // `jjfx hooks [install|status]` is global - it manages ~/.claude/settings.json
    // and needs no jj repo, so it runs before repo discovery.
    if args.first().map(String::as_str) == Some("hooks") {
        return hooks::run_cli(args.get(1).map(String::as_str));
    }

    let cwd = std::env::current_dir().context("reading current directory")?;
    let repo_root = repo::discover(&cwd)?;

    // Headless mode: dump the reconciled store and exit. Useful for scripting and
    // for confirming the store/cache round-trip without a terminal.
    if args.iter().any(|a| a == "--list") {
        let store = Store::load(&repo_root);
        let mut out = std::io::stdout().lock();
        for w in &store.workspaces {
            let path = w
                .path
                .as_deref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            writeln!(out, "{}\t{}", w.name, path)?;
        }
        return Ok(());
    }

    run_tui(repo_root).await
}

async fn run_tui(repo_root: std::path::PathBuf) -> anyhow::Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();

    // Bound the event log, then reconstruct current agent state by replaying it
    // (ADR 0004). Rotate before replay so the read is already within the cap.
    let log = events::log_path();
    events::rotate_if_needed(&log, events::MAX_LOG_BYTES).ok();
    let initial_agents = agent::fold(events::read_events(&log));

    // Filesystem watcher -> Msg::Reload. Held for the duration of the loop.
    let _watcher = watch::watch_repo(&repo_root, tx.clone())?;
    // Event-log tailer -> Msg::AgentEvent for each new line. Also held alive.
    let _log_watcher = events::watch_log(&log, tx.clone())?;

    // Work-lifecycle poller: recompute the jj/gh work state periodically and on
    // demand, sending Msg::WorkSnapshot. `work_tx` nudges it when the repo changes.
    let (work_tx, work_rx) = mpsc::unbounded_channel::<()>();
    spawn_work_poller(repo_root.clone(), tx.clone(), work_rx);

    // Blocking terminal-input reader on its own thread -> Msg::Input.
    spawn_input_reader(tx.clone());

    let mut terminal = tui::init()?;
    let result = event_loop(
        &mut terminal,
        &mut rx,
        &repo_root,
        initial_agents,
        work_tx,
        tx,
    )
    .await;

    // Always restore, then surface any loop error.
    tui::restore()?;
    terminal.show_cursor().ok();
    result
}

async fn event_loop(
    terminal: &mut tui::Tui,
    rx: &mut mpsc::UnboundedReceiver<Msg>,
    repo_root: &std::path::Path,
    initial_agents: std::collections::HashMap<std::path::PathBuf, agent::AgentState>,
    work_tx: UnboundedSender<()>,
    tx: UnboundedSender<Msg>,
) -> anyhow::Result<()> {
    let mut app = App::new(
        Store::load(repo_root),
        initial_agents,
        Box::new(terminal::KittyTerminal),
        tx,
    );
    terminal.draw(|f| app.render(f))?;

    while let Some(first) = rx.recv().await {
        // Drain everything already queued, then act once: input events in order,
        // but the many filesystem events a single jj command emits collapse into
        // one reload.
        let mut needs_reload = false;
        let mut batch = vec![first];
        while let Ok(next) = rx.try_recv() {
            batch.push(next);
        }
        for msg in batch {
            match msg {
                Msg::Reload => needs_reload = true,
                input => app.handle(input),
            }
        }
        if needs_reload {
            app.handle(Msg::Reload);
            // A repo change may have altered the work state; nudge the poller so
            // the row refreshes without waiting for the next interval tick.
            let _ = work_tx.send(());
        }

        if app.should_quit {
            break;
        }
        terminal.draw(|f| app.render(f))?;
    }
    Ok(())
}

/// Recompute the work-lifecycle snapshot on an interval and whenever nudged via
/// `refresh_rx`, sending each result as [`Msg::WorkSnapshot`]. The jj/gh reads
/// are blocking, so they run on `spawn_blocking`.
fn spawn_work_poller(
    repo_root: std::path::PathBuf,
    tx: UnboundedSender<Msg>,
    mut refresh_rx: mpsc::UnboundedReceiver<()>,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
        loop {
            let root = repo_root.clone();
            let snapshot = tokio::task::spawn_blocking(move || {
                let mut names = jj::workspace_names(&root);
                if !names.iter().any(|n| n == store::DEFAULT_WORKSPACE) {
                    names.push(store::DEFAULT_WORKSPACE.to_string());
                }
                work::snapshot(&root, &names)
            })
            .await;
            if let Ok(snapshot) = snapshot
                && tx.send(Msg::WorkSnapshot(snapshot)).is_err()
            {
                break; // app gone
            }
            // Wait for the next tick or an on-demand refresh, whichever comes first.
            tokio::select! {
                _ = interval.tick() => {}
                got = refresh_rx.recv() => {
                    if got.is_none() {
                        break; // sender dropped at shutdown
                    }
                }
            }
        }
    });
}

/// Read terminal events on a dedicated OS thread (crossterm reads block) and
/// forward them to the async loop. The thread exits when the channel closes.
fn spawn_input_reader(tx: UnboundedSender<Msg>) {
    std::thread::spawn(move || {
        // A read error (e.g. stdin closed) ends the while-let and the thread.
        while let Ok(ev) = event::read() {
            if tx.send(Msg::Input(ev)).is_err() {
                break;
            }
        }
    });
}
