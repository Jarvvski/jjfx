//! Terminal lifecycle: enter/leave the alternate screen and raw mode, with a
//! panic hook and a fatal-signal handler that restore the terminal first, so a
//! crash or an external `kill` never leaves the user's shell in raw mode or on
//! the alternate screen.

use std::io::{self, Stdout};
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Once, OnceLock};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

static ALTERNATE_SCREEN: AtomicBool = AtomicBool::new(false);
static BRACKETED_PASTE: AtomicBool = AtomicBool::new(false);
static KEYBOARD_ENHANCEMENT: AtomicBool = AtomicBool::new(false);

/// Counts `restore` calls so a test can observe that the panic hook restored the
/// terminal *without* setting one of the mode flags: that would make `restore`
/// write escape sequences to the test process's real stdout and corrupt the test
/// runner's output.
#[cfg(test)]
static RESTORES: AtomicUsize = AtomicUsize::new(0);

/// Owns an entered terminal until normal restoration completes.
///
/// The guard is the internal seam for the interactive launcher: every early
/// return, error, and panic has one cleanup owner. The public launcher only
/// exposes the complete TUI behavior, not these terminal operations.
pub struct Session {
    terminal: Option<Tui>,
    restored: bool,
}

impl Session {
    /// Enter raw mode and the alternate screen transactionally.
    ///
    /// The panic hook and the signal handler are installed **before** the
    /// terminal is touched, so a failure or a fatal signal during setup is still
    /// restored - the setup path is exactly where a broken terminal would
    /// otherwise be most likely.
    pub fn enter() -> io::Result<Self> {
        install_panic_hook();
        install_signal_restore()?;
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let _ = restore();
            return Err(error);
        }
        ALTERNATE_SCREEN.store(true, Ordering::Release);
        if let Err(error) = execute!(stdout, EnableBracketedPaste) {
            let _ = restore();
            return Err(error);
        }
        BRACKETED_PASTE.store(true, Ordering::Release);
        if let Err(error) = execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        ) {
            let _ = restore();
            return Err(error);
        }
        KEYBOARD_ENHANCEMENT.store(true, Ordering::Release);
        let terminal = match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(terminal) => terminal,
            Err(error) => {
                let _ = restore();
                return Err(error);
            }
        };
        Ok(Self {
            terminal: Some(terminal),
            restored: false,
        })
    }

    /// Borrow the active terminal for drawing and input-driven event loops.
    pub fn terminal_mut(&mut self) -> &mut Tui {
        self.terminal
            .as_mut()
            .expect("entered terminal session must own a terminal")
    }

    /// Restore the terminal and preserve the first cleanup error.
    pub fn restore(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }
        if let Some(terminal) = self.terminal.as_mut() {
            terminal.show_cursor().ok();
        }
        let result = restore();
        self.restored = true;
        result
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Leave the alternate screen, input modes, and raw mode. Safe to call more
/// than once, and every enabled mode is attempted even if an earlier cleanup
/// operation fails.
pub fn restore() -> io::Result<()> {
    #[cfg(test)]
    RESTORES.fetch_add(1, Ordering::Relaxed);
    let alternate_screen = ALTERNATE_SCREEN.swap(false, Ordering::AcqRel);
    let bracketed_paste = BRACKETED_PASTE.swap(false, Ordering::AcqRel);
    let keyboard_enhancement = KEYBOARD_ENHANCEMENT.swap(false, Ordering::AcqRel);
    let mut result = disable_raw_mode();
    let mut stdout = io::stdout();

    if bracketed_paste {
        remember_first_error(&mut result, execute!(stdout, DisableBracketedPaste));
    }
    if keyboard_enhancement {
        remember_first_error(&mut result, execute!(stdout, PopKeyboardEnhancementFlags));
    }
    if alternate_screen {
        remember_first_error(&mut result, execute!(stdout, LeaveAlternateScreen));
    }
    result
}

fn remember_first_error(result: &mut io::Result<()>, next: io::Result<()>) {
    if result.is_ok() && next.is_err() {
        *result = next;
    }
}

/// Chain terminal restoration in front of the existing panic hook once per
/// process. Repeated launcher calls must not stack duplicate hooks.
fn install_panic_hook() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = restore();
            previous(info);
        }));
    });
}

/// The signals whose default action terminates the process without running any
/// cleanup, which would strand the user's shell on the alternate screen in raw
/// mode. SIGINT is included because raw mode delivers Ctrl+C as input, so the
/// only way it arrives as a signal is an external `kill -INT`.
const FATAL_SIGNALS: [i32; 3] = [SIGINT, SIGTERM, SIGHUP];

/// Restore the terminal before a fatal signal kills the process.
///
/// The handler is deliberately *not* a raw `sigaction` callback: `unsafe` is
/// forbidden crate-wide and [`restore`] is not async-signal-safe (it takes a
/// mutex, formats escape sequences, and allocates). `signal-hook` instead
/// delivers the signal to a dedicated thread, so restoration runs in ordinary
/// thread context.
///
/// Installed once per process; the first outcome is remembered, so a repeated
/// call reports the same result rather than silently succeeding.
fn install_signal_restore() -> io::Result<()> {
    static INSTALLED: OnceLock<Result<(), String>> = OnceLock::new();
    match INSTALLED.get_or_init(|| {
        let mut signals = Signals::new(FATAL_SIGNALS).map_err(|error| error.to_string())?;
        std::thread::Builder::new()
            .name("jjfx-signals".to_string())
            .spawn(move || {
                // `forever` yields each registered signal; every one is fatal.
                if let Some(signal) = signals.forever().next() {
                    let _ = restore();
                    // Re-raise under the default disposition so a supervisor sees
                    // the process die *from the signal* (128 + n) rather than a
                    // plain exit that hides what happened. This terminates, so
                    // the fallback below only runs if the re-raise failed.
                    if signal_hook::low_level::emulate_default_handler(signal).is_err() {
                        std::process::exit(128 + signal);
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(())
    }) {
        Ok(()) => Ok(()),
        Err(message) => Err(io::Error::other(message.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialises the tests that observe the process-global restore counter, so
    /// a restore from the other test cannot stand in for the hook's own.
    static RESTORE_OBSERVERS: Mutex<()> = Mutex::new(());

    /// `restore` runs from the panic hook and the signal thread, either of which
    /// may fire before any mode was enabled, so it has to be a harmless no-op
    /// there - that is what makes installing them first safe.
    #[test]
    fn restore_is_a_no_op_before_any_mode_was_enabled() {
        let _serialised = RESTORE_OBSERVERS.lock().unwrap();
        restore().expect("restoring an untouched terminal must succeed");
        restore().expect("restoring an already-restored terminal must succeed");
    }

    /// A panic must restore the terminal, including one that happens during
    /// setup - which is why `Session::enter` installs the hook before it touches
    /// the terminal at all.
    #[test]
    fn a_panic_restores_the_terminal() {
        let _serialised = RESTORE_OBSERVERS.lock().unwrap();
        install_panic_hook();
        let before = RESTORES.load(Ordering::Relaxed);
        let panicked = std::panic::catch_unwind(|| panic!("intentional restoration test panic"));
        assert!(panicked.is_err(), "the panic must propagate");
        assert_eq!(
            RESTORES.load(Ordering::Relaxed),
            before + 1,
            "the panic hook must restore the terminal exactly once"
        );
    }

    /// Installing twice must not stack handlers or lose the first failure.
    #[test]
    fn installing_signal_restore_is_idempotent() {
        install_signal_restore().expect("signal restore should install");
        install_signal_restore().expect("a repeated install reports the first outcome");
    }
}
