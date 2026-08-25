//! Terminal lifecycle: enter/leave the alternate screen and raw mode, with a
//! panic hook that restores the terminal first so a crash never leaves the
//! user's shell in raw mode or on the alternate screen.

use std::io::{self, Stdout};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

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

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

static ALTERNATE_SCREEN: AtomicBool = AtomicBool::new(false);
static BRACKETED_PASTE: AtomicBool = AtomicBool::new(false);
static KEYBOARD_ENHANCEMENT: AtomicBool = AtomicBool::new(false);

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
    pub fn enter() -> io::Result<Self> {
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
        install_panic_hook();
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
