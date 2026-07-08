//! jjfx - keyboard-driven mission-control for parallel Claude Code agents, each
//! in its own Jujutsu workspace. It loads the authoritative workspace store,
//! mirrors `.jj/ws-cache`, event-sources each workspace's agent lifecycle from
//! Claude Code hooks, and renders an Attention-first list.

mod agent;
mod app;
mod cache;
mod events;
mod hooks;
mod jj;
mod repo;
mod store;
mod tui;
mod watch;

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

    // Blocking terminal-input reader on its own thread -> Msg::Input.
    spawn_input_reader(tx);

    let mut terminal = tui::init()?;
    let result = event_loop(&mut terminal, &mut rx, &repo_root, initial_agents).await;

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
) -> anyhow::Result<()> {
    let mut app = App::new(Store::load(repo_root), initial_agents);
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
        }

        if app.should_quit {
            break;
        }
        terminal.draw(|f| app.render(f))?;
    }
    Ok(())
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
