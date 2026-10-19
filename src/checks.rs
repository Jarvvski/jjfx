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

use serde::{Deserialize, Serialize};

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

    /// The durable form of this outcome, recorded against the Run whose change
    /// the check ran on, so a Ticket's status can reflect the check rather than
    /// the agent's own report (ticket 02). It keeps only the tool's summary line:
    /// the full output belongs in the forge footer, not in every Run Record.
    pub(crate) fn run_check(&self) -> RunCheck {
        match self {
            Outcome::Passed => RunCheck::Passed,
            Outcome::NotConfigured => RunCheck::NotConfigured,
            Outcome::Failed { command, output } => RunCheck::Failed {
                command: command.clone(),
                detail: last_line(output),
            },
            Outcome::CouldNotRun { command, detail } => RunCheck::CouldNotRun {
                command: command.clone(),
                detail: last_line(detail),
            },
        }
    }
}

/// The repository's own verification of one Run's change, as persisted on the
/// Run Record: whether the repo-defined check ran and passed, or why it did not.
///
/// A Run Record without one - an older record, or a Run that failed - is not
/// verified. A local Ticket reaches its done status only on [`RunCheck::Passed`];
/// the other variants keep it demonstrably not-done and name what happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunCheck {
    /// The check ran and exited zero.
    Passed,
    /// The check ran and failed.
    Failed {
        /// The command the repository ran.
        command: String,
        /// The tool's own summary of the failure.
        detail: String,
    },
    /// The check could not be run at all (a missing command, a timeout).
    CouldNotRun {
        /// The command the repository named.
        command: String,
        /// Why it could not run.
        detail: String,
    },
    /// No check is configured for this repository, so nothing was verified.
    NotConfigured,
}

impl RunCheck {
    /// Whether the change passed the repository's check. Only this reads as
    /// verified; every other variant - including "not configured" - does not.
    pub const fn passed(&self) -> bool {
        matches!(self, Self::Passed)
    }

    /// Why this Run is not verified, in one line, naming the command; `None`
    /// when it passed.
    pub fn reason(&self) -> Option<String> {
        match self {
            Self::Passed => None,
            Self::Failed { command, detail } => {
                Some(with_output(format!("check: `{command}` failed"), detail))
            }
            Self::CouldNotRun { command, detail } => Some(with_output(
                format!("check: `{command}` could not run"),
                detail,
            )),
            Self::NotConfigured => Some("no check is configured for this repository".to_owned()),
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
    let line = last_line(output);
    if line.is_empty() {
        return lead;
    }
    format!("{lead}: {line}")
}

/// The last non-empty, trimmed, bounded line of `output` - the tool's own
/// summary of what happened. Empty when there is no output at all.
fn last_line(output: &str) -> String {
    output
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map(|line| line.chars().take(MAX_REASON_CHARS).collect())
        .unwrap_or_default()
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

    #[test]
    fn run_check_maps_each_outcome_to_its_durable_form() {
        assert_eq!(run(&cwd(), &config("true")).run_check(), RunCheck::Passed);
        assert_eq!(
            run(&cwd(), &ChecksConfig::default()).run_check(),
            RunCheck::NotConfigured
        );

        let failed = run(&cwd(), &config("echo 'boom: type error' >&2; exit 1")).run_check();
        let RunCheck::Failed { command, detail } = &failed else {
            panic!("a non-zero exit maps to Failed, got {failed:?}");
        };
        assert_eq!(command, "echo 'boom: type error' >&2; exit 1");
        assert!(detail.contains("boom: type error"), "{detail}");
        assert!(!failed.passed());
        assert!(
            failed
                .reason()
                .expect("a failure names why")
                .contains("boom"),
            "{failed:?}"
        );

        let missing = run(&cwd(), &config("jjfx-no-such-program-exists")).run_check();
        assert!(
            matches!(missing, RunCheck::CouldNotRun { .. }),
            "a command that does not exist is not a pass: {missing:?}"
        );
        assert!(!missing.passed());
    }

    /// The durable form keeps the tool's own summary line, bounded, so a Run
    /// Record never carries a whole compiler log.
    #[test]
    fn run_check_keeps_only_the_bounded_summary_line() {
        let failed = run(
            &cwd(),
            &config("printf 'a\\nb\\nerror: could not compile\\n' >&2; exit 1"),
        )
        .run_check();
        let RunCheck::Failed { detail, .. } = &failed else {
            panic!("expected Failed, got {failed:?}");
        };
        assert_eq!(detail, "error: could not compile");
        assert!(!detail.contains('\n'));

        // Not configured and passed never carry a reason.
        assert!(RunCheck::Passed.reason().is_none());
        assert_eq!(
            RunCheck::NotConfigured.reason().as_deref(),
            Some("no check is configured for this repository")
        );
    }
}
