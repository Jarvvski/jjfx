//! Foreground Run terminal handover.
//!
//! A foreground Run owns its process group, so a failed launch or a cross-process
//! Reset can end its whole tree. That moves it out of the terminal's foreground
//! group, so it could no longer read the terminal, and the supervisor - which
//! stays in the background - could not take the terminal back without `SIGTTOU`.
//!
//! Instead the terminal is handed over and handed back by the Run's own group.
//! The supervisor spawns [`WRAPPER_SUBCOMMAND`] with `process_group(0)` and makes
//! its group the terminal's foreground group. The wrapper runs the Agent command
//! in that same group, waits for it, then makes the captured group the foreground
//! group again. Because the wrapper is the foreground group while it hands back,
//! `tcsetpgrp` never raises `SIGTTOU`.

use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::{Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use rustix::process::{Pid, getpgrp};
use rustix::termios::{tcgetpgrp, tcsetpgrp};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

/// The hidden subcommand that runs an Agent command inside the Run's group.
///
/// Both binaries dispatch it, but it never appears in user-facing help.
pub const WRAPPER_SUBCOMMAND: &str = "__foreground";

/// Overrides the executable re-invoked as the wrapper.
///
/// Production re-invokes [`std::env::current_exe`]. A test that calls the Run
/// supervisor from its own test binary - where `current_exe` is not jjfx - points
/// this at the real binary instead.
pub const FOREGROUND_WRAPPER_ENV: &str = "JJFX_FOREGROUND_WRAPPER";

/// The terminal handover for one foreground Run.
pub(crate) struct Handover {
    foreground: Pid,
}

impl Handover {
    /// Captures the terminal's foreground group, or `None` when there is nothing
    /// to hand over.
    ///
    /// `None` means `stdin` is not a terminal, or this process is not the
    /// terminal's foreground group. A background jjfx must not seize the
    /// terminal, and calling `tcsetpgrp` from a background group is exactly the
    /// path that raises `SIGTTOU`.
    pub(crate) fn capture() -> Option<Self> {
        let stdin = io::stdin();
        if !stdin.is_terminal() {
            return None;
        }
        let foreground = tcgetpgrp(&stdin).ok()?;
        (foreground == getpgrp()).then_some(Self { foreground })
    }

    /// The foreground group the wrapper restores.
    fn foreground(&self) -> Pid {
        self.foreground
    }

    /// Makes `group` the terminal's foreground group.
    ///
    /// The caller must currently be the terminal's foreground group, which
    /// [`Handover::capture`] established, so this never raises `SIGTTOU`.
    pub(crate) fn give_to(&self, group: u32) -> io::Result<()> {
        let group = i32::try_from(group)
            .ok()
            .and_then(Pid::from_raw)
            .ok_or_else(|| io::Error::other("Run PID does not fit a Unix process ID"))?;
        tcsetpgrp(io::stdin(), group).map_err(io::Error::from)
    }
}

/// Builds the command that runs `agent` inside the Run's process group.
pub(crate) fn wrapper_command(agent: &Command, handover: &Handover) -> io::Result<Command> {
    let executable =
        match std::env::var_os(FOREGROUND_WRAPPER_ENV).filter(|value| !value.is_empty()) {
            Some(executable) => PathBuf::from(executable),
            None => std::env::current_exe()?,
        };
    let mut command = Command::new(executable);
    command.arg(WRAPPER_SUBCOMMAND);
    command.arg(handover.foreground().as_raw_nonzero().get().to_string());
    command.arg("--");
    command.arg(agent.get_program());
    command.args(agent.get_args());
    Ok(command)
}

/// Runs the hidden wrapper:
/// `jjfx __foreground <foreground-group> -- <program> [args...]`.
pub fn run_wrapper(arguments: &[String]) -> anyhow::Result<()> {
    let request = WrapperRequest::parse(arguments)?;
    let handed = io::stdin().is_terminal();
    if handed {
        install_signal_handback(request.foreground);
        wait_until_foreground()?;
    }
    // The guard hands the terminal back on every Rust exit path, including a
    // failed spawn and a panic; the signal thread covers an external kill.
    let handback = handed.then_some(HandBack {
        foreground: request.foreground,
    });
    let mut agent = Command::new(&request.program)
        .args(&request.args)
        .spawn()
        .with_context(|| format!("cannot start {}", request.program))?;
    let status = agent
        .wait()
        .with_context(|| format!("cannot wait for {}", request.program))?;
    drop(handback);
    exit_like(status);
}

/// The wrapper's own arguments.
struct WrapperRequest {
    foreground: Pid,
    program: String,
    args: Vec<String>,
}

impl WrapperRequest {
    fn parse(arguments: &[String]) -> anyhow::Result<Self> {
        let mut arguments = arguments.iter();
        let foreground = arguments
            .next()
            .context("missing foreground process group")?
            .parse::<i32>()
            .ok()
            .and_then(Pid::from_raw)
            .context("invalid foreground process group")?;
        if arguments.next().map(String::as_str) != Some("--") {
            bail!("missing -- before the Agent command");
        }
        let program = arguments.next().context("missing Agent program")?.clone();
        Ok(Self {
            foreground,
            program,
            args: arguments.cloned().collect(),
        })
    }
}

/// Restores the terminal's foreground group when the wrapper leaves.
struct HandBack {
    foreground: Pid,
}

impl Drop for HandBack {
    fn drop(&mut self) {
        give_back(self.foreground);
    }
}

/// Makes `foreground` the terminal's foreground group, but only while this
/// process is itself the foreground group.
///
/// The guard is what lets the wrapper be spawned before the supervisor hands the
/// terminal over: a wrapper that never became foreground leaves the terminal
/// alone instead of trying to seize it from the background.
fn give_back(foreground: Pid) {
    let stdin = io::stdin();
    if tcgetpgrp(&stdin).ok() == Some(getpgrp()) {
        let _ = tcsetpgrp(&stdin, foreground);
    }
}

/// Blocks until the supervisor makes this process's group the terminal's
/// foreground group.
///
/// Waiting here means the Agent starts with the terminal already handed over, so
/// a fast Agent cannot exit before the handover and leave the supervisor handing
/// the terminal to a dead group.
fn wait_until_foreground() -> anyhow::Result<()> {
    let stdin = io::stdin();
    let own = getpgrp();
    let deadline = Instant::now() + Duration::from_secs(10);
    while tcgetpgrp(&stdin).ok() != Some(own) {
        if Instant::now() >= deadline {
            bail!("the Run was never given the terminal");
        }
        thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

/// Restores the terminal before an external termination kills the wrapper, so a
/// Reset that signals the Run's group never strands the supervisor in a dead
/// foreground group.
///
/// SIGINT is caught and ignored: the wrapper must survive it so the Agent decides
/// whether to stop, and the guard still restores the terminal when the Agent ends.
fn install_signal_handback(foreground: Pid) {
    let Ok(mut signals) = Signals::new([SIGINT, SIGTERM, SIGHUP]) else {
        return;
    };
    thread::spawn(move || {
        for signal in signals.forever() {
            if signal == SIGINT {
                continue;
            }
            give_back(foreground);
            if signal_hook::low_level::emulate_default_handler(signal).is_err() {
                std::process::exit(128 + signal);
            }
        }
    });
}

/// Terminates the wrapper exactly as `status` describes, so the supervisor sees
/// the Agent's own exit code or terminating signal.
fn exit_like(status: ExitStatus) -> ! {
    use std::os::unix::process::ExitStatusExt;

    if let Some(code) = status.code() {
        std::process::exit(code);
    }
    let signal = status.signal().unwrap_or(SIGTERM);
    if signal_hook::low_level::emulate_default_handler(signal).is_err() {
        std::process::exit(128 + signal);
    }
    std::process::exit(1);
}
