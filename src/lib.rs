//! Shared jjfx application implementation and interactive launcher.

mod agent;
mod app;
mod attention;
mod cli;
mod cmd;
mod config;
mod diff;
mod diff_view;
mod display;
pub mod engine;
mod events;
mod forge;
mod graph;
mod hooks;
mod jj;
mod layout;
mod pool_session;
mod prs;
mod render;
mod repo;
mod reviewer_source;
mod runtime;
mod store;
mod task_editor;
mod terminal;
mod ticket_source;
mod trunk;
mod tui;
mod ui_state;
mod viewport;
mod watch;
mod work;
mod workspace_dispatch;
mod workspace_list;

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context;
use ratatui::crossterm::event::{self, Event};
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::app::{App, Msg};
use crate::runtime::Runtime;
use crate::store::Store;

#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod test_support {
    /// Enter the terminal and panic, for an external PTY restoration test.
    pub fn panic_after_tui_init() -> ! {
        let _session = crate::tui::Session::enter().expect("test PTY should enter");
        panic!("intentional terminal restoration test panic");
    }

    /// Enter the terminal and hold it, for an external PTY signal test. The
    /// process stays in raw mode on the alternate screen until the test signals
    /// it, so the handler's restoration is the only thing that can clean up.
    pub fn block_after_tui_init() -> ! {
        let _session = crate::tui::Session::enter().expect("test PTY should enter");
        loop {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

/// Run the jjfx binary's argument router.
pub fn run(args: Vec<String>) -> anyhow::Result<()> {
    // The four `cli::run` sites below differ only in the `args` they forward, so
    // bind the shared dispatch once and call it at each.
    let dispatch = |args: Vec<String>| {
        cli::run(
            args,
            launch,
            terminal_settings,
            colocate_setting,
            open_workspaces,
            tickets_setting,
            spend_setting,
        )
    };

    // The hidden foreground wrapper is dispatched before anything else: it is
    // spawned with an Agent command's own arguments, which must not be scanned as
    // jjfx's. Like `--version` it needs no repository, so it also skips repo
    // discovery - the Run's workspace need not be a jj repo.
    if args.first().map(String::as_str) == Some(engine::WRAPPER_SUBCOMMAND) {
        return dispatch(args);
    }

    // `--version`/`-V` prints "jjfx <version>" and exits before any repo or
    // terminal work, so it is safe to run outside a jj repo and without a TTY
    // (release/CI can smoke-test the built binary with it).
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    // Help is repo-independent, like --version: print usage and exit 0 even
    // outside a jj repo, so discovery stays possible.
    if matches!(
        args.first().map(String::as_str),
        Some("help" | "-h" | "--help")
    ) {
        return dispatch(args);
    }

    // `jjfx hooks [install|status]` is global - it manages ~/.claude/settings.json
    // and needs no jj repo, so it runs before repo discovery.
    if args.first().map(String::as_str) == Some("hooks") {
        return hooks::run_cli(args.get(1).map(String::as_str));
    }

    // The TUI is explicit. Bare `jjfx` is the non-interactive CLI and prints
    // help, so scripts and shell completion never unexpectedly claim the
    // terminal.
    if args.is_empty() {
        return dispatch(vec!["help".to_owned()]);
    }
    if args.first().map(String::as_str) == Some("tui") {
        if args.len() != 1 {
            anyhow::bail!("Usage: jjfx tui");
        }
        let cwd = std::env::current_dir().context("reading current directory")?;
        let repo_root = repo::discover(&cwd)?;
        return launch(repo_root);
    }

    let cwd = std::env::current_dir().context("reading current directory")?;
    let repo_root = repo::discover(&cwd)?;

    // Headless mode: dump the reconciled store and exit. Useful for scripting and
    // for confirming the store/cache round-trip without a terminal.
    if args.iter().any(|a| a == "--list") {
        let store = Store::load(&repo_root);
        let mut out = std::io::stdout().lock();
        for w in store.workspaces() {
            let path = w
                .path
                .present()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            writeln!(out, "{}\t{}", w.name, path)?;
        }
        return Ok(());
    }

    dispatch(args)
}

/// Resolve the terminal settings a `mount` invocation needs: the first-pane
/// command, wrapped in the login interactive shell (empty when unconfigured),
/// and the pane layout selected for the current display.
///
/// `jjfx`'s entrypoint hands this to the shared CLI router so
/// `mount` opens the same first pane and layout as the TUI. It is loaded lazily
/// - only a `mount` invocation reads jjfx's config file.
pub fn terminal_settings() -> anyhow::Result<crate::engine::TerminalSettings> {
    let config = config::load()?;
    let layout = layout::LayoutSettings::from_config(&config.terminal)
        .resolve(display::attached_condition())?
        .clone();
    Ok(crate::engine::TerminalSettings {
        first_pane_command: config.first_pane_command(),
        layout,
    })
}

/// Lists the workspace tabs currently open in the terminal jjfx drives.
///
/// `jjfx`'s entrypoint hands this to the shared CLI router so
/// `dispatch` can keep a Workspace that is open in a terminal out of Worker
/// selection. It is loaded lazily - only a `dispatch` invocation reads jjfx's
/// config file - and fails when the terminal cannot be queried, so the caller
/// can fail closed.
pub fn open_workspaces() -> anyhow::Result<std::collections::BTreeSet<String>> {
    use crate::terminal::Terminal as _;
    let config = config::load()?;
    let terminal = terminal::KittyTerminal::new(
        &config.terminal,
        config.agent_command(),
        config.first_pane_command(),
    );
    terminal.open_workspaces()
}

/// The `colocate` setting the CLI uses when creating workspaces. Read lazily -
/// only CLI commands that touch a repository load jjfx's config file.
pub fn colocate_setting() -> anyhow::Result<bool> {
    Ok(config::load()?.colocate)
}

/// The `[tickets]` settings the CLI uses to discover Ready Tickets. Loaded
/// lazily, and only by `dispatch` and `orchestrate`, so a bad source surfaces
/// before any Worker reservation or Pool mutation.
fn tickets_setting() -> anyhow::Result<config::TicketsConfig> {
    Ok(config::load()?.tickets)
}

/// Resolve the aggregate spend cap Dispatch is gated on.
fn spend_setting() -> anyhow::Result<config::SpendConfig> {
    Ok(config::load()?.spend)
}

/// Launch the shared interactive jjfx TUI for a discovered repository.
///
/// The launcher owns the runtime, watchers, pollers, App, and terminal
/// lifecycle. The CLI keeps versioning and command routing outside this
/// seam, so it enters exactly the same interactive implementation.
pub fn launch(repo_root: PathBuf) -> anyhow::Result<()> {
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

async fn run_tui(repo_root: PathBuf) -> anyhow::Result<()> {
    // Load jjfx's own config first: a parse error must surface here, before
    // tui::init() takes over the screen, or the message would be lost.
    let config = config::load()?;
    // Persisted UI toggles (e.g. the world-graph pane); missing/garbled is fine.
    let ui = ui_state::load();

    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();

    // Bound the event log, then reconstruct current agent state by replaying it
    // (ADR 0004). Rotate before replay so the read is already within the cap.
    // The replay also reports the offset the tailer must start from, so a hook
    // line landing between the two is neither lost nor replayed twice.
    let log = events::log_path();
    events::rotate_if_needed(&log, events::MAX_LOG_BYTES).ok();
    let (replayed, replay_offset) = events::replay(&log);
    let initial_agents = agent::AgentStates::replay(replayed);

    // Filesystem watcher -> Msg::Reload. Held for the duration of the loop.
    let _watcher = watch::watch_repo(&repo_root, tx.clone())?;
    // Event-log tailer -> Msg::AgentEvent for each new line. Also held alive.
    let _log_watcher = events::watch_log(&log, tx.clone(), replay_offset)?;

    // Work-lifecycle poller: recompute the jj/gh work state periodically and on
    // demand, sending Msg::WorkSnapshot. `work_tx` nudges it when the repo changes.
    let (work_tx, work_rx) = mpsc::unbounded_channel::<()>();
    spawn_work_poller(repo_root.clone(), tx.clone(), work_rx);

    // Blocking terminal-input reader on its own thread -> Msg::Input.
    spawn_input_reader(tx.clone());

    // Animation ticker -> Msg::Tick, driving the working-glyph bounce. The
    // shared gate is republished by the event loop after every batch, so an
    // idle TUI emits no ticks (and does no per-tick work at all).
    let animating = Arc::new(AtomicBool::new(false));
    spawn_animation_ticker(tx.clone(), Arc::clone(&animating));

    let terminal: Arc<dyn terminal::Terminal> = Arc::from(Box::new(terminal::KittyTerminal::new(
        &config.terminal,
        config.agent_command(),
        config.first_pane_command(),
    )) as Box<dyn terminal::Terminal>);
    let runtime = Runtime::new(
        tx.clone(),
        repo_root.clone(),
        config.forge,
        config.colocate,
        Arc::clone(&terminal),
        Arc::new(jj::RealJj::new(repo_root.clone())),
        Some(events::setup_error_log_path()),
    );
    let mut app = App::new(
        Store::load(&repo_root).with_colocate(config.colocate),
        initial_agents,
        terminal,
        ui.world_pane,
        tx,
        config.tickets,
        config.spend,
    );
    // A `jj`/`jj-lib` version mismatch degrades the graph read. Surface it up
    // front, before the first load, so it is visible even if that load errors
    // and never sends `GraphLoaded`.
    if let Some(notice) = crate::engine::jj_lib_compat_notice() {
        app.set_graph_notice(notice);
    }
    // A persisted-on world pane needs its first graph load requested here (the
    // load is otherwise only triggered by the toggle keys). The idle lane starts
    // folded (a launch default, not persisted state), so nothing to restore.
    app.refresh_graph_if_visible();
    for effect in app.take_effects() {
        runtime.execute(effect);
    }

    let (worker_tx, worker_rx) = mpsc::unbounded_channel::<()>();
    spawn_worker_pool_poller(app.workspace_dispatch_controller(), worker_rx);

    let mut session = tui::Session::enter()?;
    let result = event_loop(
        session.terminal_mut(),
        &mut rx,
        &mut app,
        &runtime,
        work_tx,
        worker_tx,
        &animating,
    )
    .await;
    let restore_result = session.restore();

    // Persist the UI toggles. Best-effort: a failed write of a nicety must not
    // turn a clean quit into an error.
    ui_state::save(&ui_state::UiState {
        world_pane: app.world_pane(),
    })
    .ok();

    match result {
        Ok(()) => restore_result.map_err(Into::into),
        Err(error) => Err(error),
    }
}

async fn event_loop(
    terminal: &mut tui::Tui,
    rx: &mut mpsc::UnboundedReceiver<Msg>,
    app: &mut App,
    runtime: &Runtime,
    work_tx: UnboundedSender<()>,
    worker_tx: UnboundedSender<()>,
    animating: &AtomicBool,
) -> anyhow::Result<()> {
    terminal.draw(|f| app.render(f))?;
    animating.store(app.is_animating(), Ordering::Relaxed);

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
                    let effects = app.handle(input);
                    for effect in effects {
                        runtime.execute(effect);
                    }
                    needs_redraw = true;
                }
            }
        }
        if needs_reload {
            let effects = app.handle(Msg::Reload);
            for effect in effects {
                runtime.execute(effect);
            }
            // A repo change may have altered the work state; nudge the poller so
            // the row refreshes without waiting for the next interval tick.
            let _ = work_tx.send(());
            let _ = worker_tx.send(());
            needs_redraw = true;
        }

        if app.should_quit {
            break;
        }
        if needs_redraw {
            terminal.draw(|f| app.render(f))?;
        }
        // Publish the animation gate after every batch: the ticker keeps
        // sending only while something on screen is actually moving.
        animating.store(app.is_animating(), Ordering::Relaxed);
    }
    Ok(())
}

/// Recompute the work-lifecycle snapshot on an interval and whenever nudged via
/// `refresh_rx`, sending each result as [`Msg::WorkSnapshot`]. The jj/gh reads
/// are blocking, so they run on `spawn_blocking`.
///
/// `jj::workspace_names` reports a typed error rather than an empty list when jj
/// cannot be read. The poller cannot surface that from inside the alt-screen, so
/// it degrades deliberately: it reuses the last good names so a transient failure
/// does not blank the work list, and retries on the next tick.
fn spawn_work_poller(
    repo_root: PathBuf,
    tx: UnboundedSender<Msg>,
    mut refresh_rx: mpsc::UnboundedReceiver<()>,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
        let mut last_names: Vec<String> = Vec::new();
        loop {
            let root = repo_root.clone();
            let previous = last_names.clone();
            let polled = tokio::task::spawn_blocking(move || {
                let mut names = jj::workspace_names(&root).unwrap_or(previous);
                if !names.iter().any(|n| n == store::DEFAULT_WORKSPACE) {
                    names.push(store::DEFAULT_WORKSPACE.to_string());
                }
                let snapshot = work::snapshot(&work::SystemWorkReads::new(root), &names);
                (names, snapshot)
            })
            .await;
            if let Ok((names, snapshot)) = polled {
                last_names = names;
                if tx.send(Msg::WorkSnapshot(snapshot)).is_err() {
                    break;
                }
            }
            tokio::select! {
                _ = interval.tick() => {}
                got = refresh_rx.recv() => {
                    if got.is_none() {
                        break;
                    }
                }
            }
        }
    });
}

/// Ask the Workspace Dispatch controller to reconcile the compatible Worker
/// Pool periodically and after repository changes.
fn spawn_worker_pool_poller(
    controller: crate::workspace_dispatch::WorkspaceDispatchController,
    mut refresh_rx: mpsc::UnboundedReceiver<()>,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
        loop {
            controller.submit(
                crate::workspace_dispatch::WorkspaceDispatchCommand::Refresh { operation: 0 },
            );
            tokio::select! {
                _ = interval.tick() => {}
                got = refresh_rx.recv() => {
                    if got.is_none() {
                        break;
                    }
                }
            }
        }
    });
}

/// Send [`Msg::Tick`] every 150ms while something on screen is animating. The
/// gate is a shared flag the event loop republishes after each batch, so an
/// idle TUI (nothing working, nothing pending) emits no ticks and the loop
/// stays parked on its message channel instead of waking ~7 times a second.
fn spawn_animation_ticker(tx: UnboundedSender<Msg>, animating: Arc<AtomicBool>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(150));
        loop {
            interval.tick().await;
            if !animating.load(Ordering::Relaxed) {
                continue;
            }
            if tx.send(Msg::Tick).is_err() {
                break;
            }
        }
    });
}

/// What the input reader does with one `event::read()` outcome.
#[derive(Debug, PartialEq, Eq)]
enum ReadStep {
    /// Forward the event to the app.
    Forward(Event),
    /// The read was interrupted - typically a signal arriving mid-read. The
    /// event is still pending, so retrying loses nothing.
    Retry,
    /// The read failed for good. The reader cannot recover in-process, so the
    /// app is told instead of the thread dying silently.
    Fatal(String),
}

fn classify_read(result: io::Result<Event>) -> ReadStep {
    match result {
        Ok(event) => ReadStep::Forward(event),
        Err(error) if error.kind() == io::ErrorKind::Interrupted => ReadStep::Retry,
        Err(error) => ReadStep::Fatal(error.to_string()),
    }
}

/// Read terminal events on a dedicated OS thread (crossterm reads block) and
/// forward them to the async loop. The thread exits when the channel closes.
///
/// A read error must not silently end the thread: the TUI keeps rendering, so
/// an input-dead session looks exactly like a frozen one. Transient errors are
/// retried and a fatal one is surfaced as [`Msg::InputFailed`].
///
/// The thread is deliberately never joined. It is parked in a blocking
/// `event::read()`, so joining would hang until the next keypress; the thread
/// either exits on its own when the receiver is dropped or dies with the
/// process, and it holds no state worth flushing.
fn spawn_input_reader(tx: UnboundedSender<Msg>) {
    std::thread::spawn(move || {
        loop {
            match classify_read(event::read()) {
                ReadStep::Forward(event) => {
                    if tx.send(Msg::Input(event)).is_err() {
                        return;
                    }
                }
                ReadStep::Retry => continue,
                ReadStep::Fatal(message) => {
                    let _ = tx.send(Msg::InputFailed(message));
                    return;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The gate is the whole point of the ticker: idle means silent. The
    /// timeouts are generous multiples of the 150ms interval so a busy CI box
    /// cannot turn a scheduling delay into a false failure.
    #[tokio::test]
    async fn animation_ticker_is_silent_until_something_animates() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
        let animating = Arc::new(AtomicBool::new(false));
        spawn_animation_ticker(tx, Arc::clone(&animating));

        assert!(
            tokio::time::timeout(Duration::from_millis(600), rx.recv())
                .await
                .is_err(),
            "an idle ticker must not emit ticks"
        );

        animating.store(true, Ordering::Relaxed);
        assert!(
            matches!(
                tokio::time::timeout(Duration::from_millis(600), rx.recv()).await,
                Ok(Some(Msg::Tick))
            ),
            "ticks must resume once something animates"
        );
    }

    /// A signal arriving mid-read must not kill the reader: the event is still
    /// pending, so that read is retried rather than treated as fatal. Anything
    /// else is surfaced, because the thread cannot recover in-process.
    #[test]
    fn an_interrupted_read_is_retried_and_a_fatal_one_is_surfaced() {
        assert_eq!(
            classify_read(Err(io::Error::from(io::ErrorKind::Interrupted))),
            ReadStep::Retry
        );

        // A fatal error keeps its own reason, so the footer can show it.
        let error = io::Error::from(io::ErrorKind::PermissionDenied);
        let reason = error.to_string();
        assert_eq!(classify_read(Err(error)), ReadStep::Fatal(reason));

        let event = Event::FocusGained;
        assert_eq!(classify_read(Ok(event.clone())), ReadStep::Forward(event));
    }
}
