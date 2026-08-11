//! Terminal lifecycle: enter/leave the alternate screen and raw mode, with a
//! panic hook that restores the terminal first so a crash never leaves the
//! user's shell in raw mode or on the alternate screen.

use std::io::{self, Stdout};
use std::sync::Once;

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

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

/// Leave the alternate screen and raw mode. Safe to call more than once, and
/// both operations are attempted even if the first one fails.
pub fn restore() -> io::Result<()> {
    let raw_mode = disable_raw_mode();
    let alternate_screen = execute!(io::stdout(), LeaveAlternateScreen);
    match (raw_mode, alternate_screen) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(raw_error), Err(_alternate_error)) => Err(raw_error),
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
