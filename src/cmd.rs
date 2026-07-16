//! Central subprocess runner. Every external command jjfx spawns goes through
//! [`cmd`], which always finishes by capturing both output streams - a bare
//! `Command::status()` inherits the parent's stdout/stderr and prints straight
//! onto the alt-screen TUI, corrupting it (the forge bug fixed in v0.8.2).
//! Capturing makes that class of regression structurally impossible: there is no
//! builder method that leaves the streams inherited.
//!
//! The one deliberate exception is a probe that must *write* to the child's
//! stdin (Forge's GPG adapter); it spawns `Command` directly but still nulls
//! both output streams. `.output()` closes stdin, so it cannot host that case.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow};

/// A command builder that always captures output. Chain `arg`/`args`/
/// `current_dir`/`env`, then finish with [`run`](Cmd::run).
#[must_use = "a Cmd does nothing until `.run()` is called"]
pub struct Cmd {
    inner: Command,
    /// Human-readable `program arg arg ...`, used in error messages.
    label: String,
}

/// Start building a captured run of `program`.
pub fn cmd(program: &str) -> Cmd {
    Cmd {
        inner: Command::new(program),
        label: program.to_string(),
    }
}

impl Cmd {
    /// Append one argument.
    pub fn arg<S: AsRef<OsStr>>(mut self, arg: S) -> Self {
        let arg = arg.as_ref();
        self.label.push(' ');
        self.label.push_str(&arg.to_string_lossy());
        self.inner.arg(arg);
        self
    }

    /// Append several arguments.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for arg in args {
            self = self.arg(arg);
        }
        self
    }

    /// Run the command in `dir`, so a jj command's `@` resolves to that
    /// workspace's own working copy.
    pub fn current_dir<P: AsRef<Path>>(mut self, dir: P) -> Self {
        self.inner.current_dir(dir);
        self
    }

    /// Spawn the command and return immediately, without waiting for it to
    /// finish, with all three standard streams nulled so nothing leaks onto the
    /// alt-screen TUI. For launching a detaching background process (e.g. a
    /// dropdown terminal) that jjfx then polls for over another channel - using
    /// [`run`](Cmd::run) would block until the child's stdout pipe closed.
    pub fn spawn_detached(mut self) -> Result<()> {
        self.inner
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("spawning {}", self.label))?;
        Ok(())
    }

    /// Run to completion, capturing both streams. Errors only when the process
    /// cannot be spawned or waited on; a non-zero exit is a completed [`Run`]
    /// with [`Run::ok`] `== false`, not an `Err`.
    pub fn run(mut self) -> Result<Run> {
        let out = self
            .inner
            .output()
            .with_context(|| format!("running {}", self.label))?;
        Ok(Run {
            label: self.label,
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// A finished command's captured result (stdout/stderr are lossy UTF-8).
#[derive(Debug)]
pub struct Run {
    label: String,
    ok: bool,
    stdout: String,
    stderr: String,
}

impl Run {
    /// Did the process exit zero?
    pub fn ok(&self) -> bool {
        self.ok
    }

    /// Captured stdout, whether or not the run succeeded.
    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    /// Captured stderr, whether or not the run succeeded. jj writes its status
    /// lines (e.g. "Nothing changed.") here, so callers that must tell a no-op
    /// from real work read this even on a zero exit.
    pub fn stderr(&self) -> &str {
        &self.stderr
    }

    /// Owned stdout when the run succeeded, else `None` - the read-only idiom for
    /// callers that degrade silently on failure.
    pub fn stdout_ok(self) -> Option<String> {
        self.ok.then_some(self.stdout)
    }

    /// Owned stdout on success; on a non-zero exit, an error carrying the command
    /// label and trimmed stderr - the mutation idiom for callers that surface
    /// failures.
    pub fn checked(self) -> Result<String> {
        if self.ok {
            Ok(self.stdout)
        } else {
            Err(anyhow!("{} failed: {}", self.label, self.stderr.trim()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_stdout_of_a_successful_run() {
        let run = cmd("printf").arg("hello").run().expect("printf spawns");
        assert!(run.ok());
        assert_eq!(run.stdout(), "hello");
    }

    #[test]
    fn non_zero_exit_is_a_completed_run_not_an_error() {
        let run = cmd("false").run().expect("false spawns");
        assert!(!run.ok());
        assert!(run.stdout_ok().is_none());
    }

    #[test]
    fn checked_carries_the_label_and_stderr_on_failure() {
        // `sh -c 'echo boom >&2; exit 1'` fails with a stderr message.
        let err = cmd("sh")
            .args(["-c", "echo boom >&2; exit 1"])
            .run()
            .expect("sh spawns")
            .checked()
            .expect_err("non-zero exit is an error");
        let msg = err.to_string();
        assert!(msg.contains("sh -c"), "label missing: {msg}");
        assert!(msg.contains("boom"), "stderr missing: {msg}");
    }

    #[test]
    fn spawning_a_missing_program_is_an_error() {
        let err = cmd("jjfx-no-such-program-exists").run().unwrap_err();
        assert!(
            err.to_string()
                .contains("running jjfx-no-such-program-exists")
        );
    }
}
