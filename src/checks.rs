//! The repo-owned build check the forge runs against the welded tree, before it
//! pushes (ticket 18).
//!
//! A Run is `done` purely from the agent runtime's own terminal event, so the
//! whole "this ticket is finished" signal is the agent's report that it is. Two
//! changes can each be green, mergeable and at zero open threads while **not
//! compiling together** - both calling a member a third, still-open change adds.
//! The forge welds onto trunk before it pushes, so a check run here sees what
//! `main` will actually be, which a post-Run hook on the un-welded tree would
//! miss.
//!
//! jjfx never guesses the command: an unconfigured repo is a no-op, not a
//! default. A check that cannot run is a failure, never a pass - the same defect
//! class as a scoping helper that exits zero having checked nothing.

use std::path::Path;
use std::time::Duration;

use crate::cmd::{Run, cmd};
use crate::config::ChecksConfig;

/// The longest reason the forge puts in its one-line footer. `Failed` and
/// `CouldNotRun` keep the whole output; this only bounds what is *shown*.
const MAX_REASON_CHARS: usize = 240;

/// What running the configured check produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// No check is configured (or it is disabled): the forge behaves exactly as
    /// it did before.
    NotConfigured,
    /// The command ran and exited zero.
    Passed,
    /// The command ran and failed. Carries the command and its output, so a
    /// caller names both rather than reporting a bare failure.
    Failed { command: String, output: String },
    /// The check could not be run at all: the command does not exist, or it
    /// outran its timeout. Never a pass.
    CouldNotRun { command: String, detail: String },
}

impl Outcome {
    /// The reason a check that did not pass must stop the push, naming the
    /// command and the line of output that says why; `None` when the step should
    /// not stop anything.
    pub(crate) fn stop_reason(&self) -> Option<String> {
        match self {
            Outcome::NotConfigured | Outcome::Passed => None,
            Outcome::Failed { command, output } => {
                Some(with_output(format!("check: `{command}` failed"), output))
            }
            Outcome::CouldNotRun { command, detail } => Some(with_output(
                format!("check: `{command}` could not run"),
                detail,
            )),
        }
    }
}

/// Run the configured check in `dir` - the workspace's working copy, which Weld
/// has already rebased onto trunk - through the user's shell, bounded by the
/// configured timeout.
///
/// The command is the repo's own shell line (`cargo check`,
/// `./gradlew :app:backend:compileKotlin`), so a repo's toolchain needs no jjfx
/// knowledge of it. A shell reports a command it cannot find as exit 127 and one
/// it cannot execute as 126; those are a check that could not run, not a build
/// that failed, and both stop the push.
pub(crate) fn run(dir: &Path, config: &ChecksConfig) -> Outcome {
    let Some(command) = config.command() else {
        return Outcome::NotConfigured;
    };
    let command = command.to_owned();
    match cmd("sh")
        .arg("-c")
        .arg(&command)
        .current_dir(dir)
        .run_with_timeout(Duration::from_secs(config.timeout_secs))
    {
        Ok(run) if run.ok() => Outcome::Passed,
        Ok(run) if is_shell_not_runnable(run.code()) => Outcome::CouldNotRun {
            command,
            detail: combined_output(&run),
        },
        Ok(run) => Outcome::Failed {
            command,
            output: combined_output(&run),
        },
        Err(error) => Outcome::CouldNotRun {
            command,
            detail: error.to_string(),
        },
    }
}

/// Whether a shell exit code means the command itself could not be executed
/// (`127` not found, `126` found but not executable) rather than that it ran and
/// reported a failure. POSIX reserves both for `sh -c`.
fn is_shell_not_runnable(code: Option<i32>) -> bool {
    matches!(code, Some(126 | 127))
}

/// The command's own output, stderr first (where compilers put diagnostics), then
/// stdout.
fn combined_output(run: &Run) -> String {
    let stderr = run.stderr().trim();
    let stdout = run.stdout().trim();
    match (stderr.is_empty(), stdout.is_empty()) {
        (false, false) => format!("{stderr}\n{stdout}"),
        (false, true) => stderr.to_owned(),
        (true, false) => stdout.to_owned(),
        (true, true) => String::new(),
    }
}

/// Join a lead with the last line of output that carries information, collapsed
/// to one line and bounded. The last line is the tool's own summary of the
/// failure ("error: could not compile ...", "command not found"), and the footer
/// is a single line, so the whole log cannot go there.
fn with_output(lead: String, output: &str) -> String {
    let Some(line) = output.lines().map(str::trim).rfind(|line| !line.is_empty()) else {
        return lead;
    };
    let line: String = line.chars().take(MAX_REASON_CHARS).collect();
    format!("{lead}: {line}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A check config running `command`.
    fn config(command: &str) -> ChecksConfig {
        ChecksConfig {
            command: Some(command.to_owned()),
            ..ChecksConfig::default()
        }
    }

    /// A directory to run a check in.
    fn cwd() -> std::path::PathBuf {
        std::env::temp_dir()
    }

    #[test]
    fn an_unconfigured_check_is_a_no_op() {
        let outcome = run(&cwd(), &ChecksConfig::default());
        assert_eq!(outcome, Outcome::NotConfigured);
        assert_eq!(outcome.stop_reason(), None);
    }

    #[test]
    fn a_disabled_check_is_a_no_op_even_with_a_command() {
        let disabled = ChecksConfig {
            command: Some("exit 1".to_owned()),
            enabled: false,
            ..ChecksConfig::default()
        };
        assert_eq!(run(&cwd(), &disabled), Outcome::NotConfigured);
    }

    #[test]
    fn a_command_that_exits_zero_passes() {
        let outcome = run(&cwd(), &config("true"));
        assert_eq!(outcome, Outcome::Passed);
        assert_eq!(outcome.stop_reason(), None);
    }

    #[test]
    fn a_command_that_exits_non_zero_fails_naming_the_command_and_output() {
        let outcome = run(&cwd(), &config("echo 'boom: type error' >&2; exit 1"));
        let Outcome::Failed { command, output } = &outcome else {
            panic!("a non-zero exit is a failure, got {outcome:?}");
        };
        assert_eq!(command, "echo 'boom: type error' >&2; exit 1");
        assert!(output.contains("boom: type error"), "{output}");
        let reason = outcome.stop_reason().expect("a failure stops the push");
        assert!(reason.contains("boom: type error"), "{reason}");
        assert!(reason.contains("failed"), "{reason}");
    }

    #[test]
    fn a_missing_command_could_not_run_rather_than_passing() {
        let outcome = run(&cwd(), &config("jjfx-no-such-program-exists"));
        assert!(
            matches!(outcome, Outcome::CouldNotRun { .. }),
            "a command that does not exist must not read as a pass: {outcome:?}"
        );
        let reason = outcome
            .stop_reason()
            .expect("a check that cannot run stops");
        assert!(reason.contains("could not run"), "{reason}");
    }

    #[test]
    fn a_check_that_outruns_its_timeout_could_not_run() {
        let outcome = run(
            &cwd(),
            &ChecksConfig {
                command: Some("sleep 30".to_owned()),
                timeout_secs: 1,
                ..ChecksConfig::default()
            },
        );
        assert!(
            matches!(outcome, Outcome::CouldNotRun { .. }),
            "a timed-out check must not read as a pass: {outcome:?}"
        );
    }

    #[test]
    fn the_reason_names_the_last_line_of_output_on_one_line() {
        let outcome = run(&cwd(), &config("printf 'a\\nb\\nboom\\n' >&2; exit 1"));
        let reason = outcome.stop_reason().expect("a failure stops the push");
        assert!(!reason.contains('\n'), "the footer is one line: {reason}");
        assert!(reason.ends_with("boom"), "{reason}");
    }

    #[test]
    fn with_output_bounds_the_line_it_shows() {
        let long = "x".repeat(1000);
        let reason = with_output("check: `cargo check` failed".to_owned(), &long);
        assert!(!reason.contains('\n'));
        assert_eq!(reason.matches('x').count(), MAX_REASON_CHARS);

        // No output at all still names the command.
        assert_eq!(
            with_output("check: `cargo check` failed".to_owned(), "  \n"),
            "check: `cargo check` failed"
        );
    }
}
