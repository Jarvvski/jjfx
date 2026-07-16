//! jjfx - keyboard-driven mission-control for parallel Claude Code agents, each
//! in its own Jujutsu workspace. It loads the authoritative workspace store,
//! mirrors `.jj/ws-cache`, event-sources each workspace's agent lifecycle from
//! Claude Code hooks, and renders an Attention-first list.

mod agent;
mod app;
mod attention;
mod cache;
mod cmd;
mod config;
mod diff;
mod diff_view;
mod events;
mod forge;
mod graph;
mod hooks;
mod jj;
mod pr;
mod prs;
mod repo;
mod store;
mod terminal;
mod trunk;
mod tui;
mod ui_state;
mod viewport;
mod watch;
mod work;
mod workspace_list;

use std::io::Write;

use anyhow::Context;
use ratatui::crossterm::event;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::app::{App, Msg};
use crate::store::Store;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // `--version`/`-V` prints "jjfx <version>" and exits before any repo or
    // terminal work, so it is safe to run outside a jj repo and without a TTY
    // (release/CI can smoke-test the built binary with it).
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

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

    // Build the runtime explicitly so shutdown can abandon in-flight blocking
    // tasks. `#[tokio::main]` drops the runtime on return, and that drop joins
    // every `spawn_blocking` thread - if the work poller had a `gh`/`jj`
    // snapshot mid-flight when the user quit, the process lingered until the
    // subprocess finished. `shutdown_background` releases those threads
    // without waiting.
    let runtime = tokio::runtime::Runtime::new().context("building tokio runtime")?;
    let result = runtime.block_on(run_tui(repo_root));
    runtime.shutdown_background();
    result
}

async fn run_tui(repo_root: std::path::PathBuf) -> anyhow::Result<()> {
    // Load jjfx's own config first: a parse error must surface here, before
    // tui::init() takes over the screen, or the message would be lost.
    let config = config::load()?;
    // Persisted UI toggles (e.g. the world-graph pane); missing/garbled is fine.
    let ui = ui_state::load();

    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();

    // Bound the event log, then reconstruct current agent state by replaying it
    // (ADR 0004). Rotate before replay so the read is already within the cap.
    let log = events::log_path();
    events::rotate_if_needed(&log, events::MAX_LOG_BYTES).ok();
    let initial_agents = agent::AgentStates::replay(events::read_events(&log));

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

    // Animation ticker -> Msg::Tick, driving the working-glyph bounce.
    spawn_animation_ticker(tx.clone());

    let mut app = App::new(
        Store::load(&repo_root),
        initial_agents,
        Box::new(terminal::KittyTerminal::new(
            &config.terminal,
            config.agent_command(),
        )),
        Box::new(jj::RealJj::new(repo_root.clone())),
        config.forge,
        ui.world_pane,
        tx,
    );
    // A persisted-on world pane needs its first graph load kicked off here (the
    // load is otherwise only triggered by the toggle keys).
    app.refresh_graph_if_visible();

    let mut terminal = tui::init()?;
    let result = event_loop(&mut terminal, &mut rx, &mut app, work_tx).await;

    // Always restore, then surface any loop error.
    tui::restore()?;
    terminal.show_cursor().ok();

    // Persist the UI toggles. Best-effort: a failed write of a nicety must not
    // turn a clean quit into an error.
    ui_state::save(&ui_state::UiState {
        world_pane: app.world_pane(),
    })
    .ok();
    result
}

async fn event_loop(
    terminal: &mut tui::Tui,
    rx: &mut mpsc::UnboundedReceiver<Msg>,
    app: &mut App,
    work_tx: UnboundedSender<()>,
) -> anyhow::Result<()> {
    terminal.draw(|f| app.render(f))?;

    while let Some(first) = rx.recv().await {
        // Drain everything already queued, then act once: input events in order,
        // but the many filesystem events a single jj command emits collapse into
        // one reload.
        let mut needs_reload = false;
        let mut needs_redraw = false;
        let mut batch = vec![first];
        while let Ok(next) = rx.try_recv() {
            batch.push(next);
        }
        for msg in batch {
            match msg {
                Msg::Reload => needs_reload = true,
                // A tick redraws only while something is animating on screen;
                // otherwise the steady tick stream would repaint an idle TUI.
                Msg::Tick => needs_redraw |= app.animate(),
                input => {
                    app.handle(input);
                    needs_redraw = true;
                }
            }
        }
        if needs_reload {
            app.handle(Msg::Reload);
            // A repo change may have altered the work state; nudge the poller so
            // the row refreshes without waiting for the next interval tick.
            let _ = work_tx.send(());
            needs_redraw = true;
        }

        if app.should_quit {
            break;
        }
        if needs_redraw {
            terminal.draw(|f| app.render(f))?;
        }
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

/// Send [`Msg::Tick`] every 150ms (jj-wsx's animation frame rate) to advance
/// the working-glyph bounce. The app decides per tick whether a redraw is
/// warranted, so an idle screen costs nothing beyond the channel send.
fn spawn_animation_ticker(tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(150));
        loop {
            interval.tick().await;
            if tx.send(Msg::Tick).is_err() {
                break; // app gone
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
