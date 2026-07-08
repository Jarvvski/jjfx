//! Terminal lifecycle: enter/leave the alternate screen and raw mode, with a
//! panic hook that restores the terminal first so a crash never leaves the
//! user's shell in raw mode or on the alternate screen.

use std::io::{self, Stdout};

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Enter raw mode + the alternate screen and install the restore-on-panic hook.
pub fn init() -> io::Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    install_panic_hook();
    Terminal::new(CrosstermBackend::new(stdout))
}

/// Leave the alternate screen and raw mode. Safe to call more than once.
pub fn restore() -> io::Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}

/// Chain terminal restoration in front of the existing panic hook, so a panic in
/// the render/event loop leaves the terminal usable before printing the message.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore();
        previous(info);
    }));
}
