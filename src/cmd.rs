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
use std::io::Read;
#[cfg(test)]
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
#[cfg(test)]
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};

/// How often a bounded run re-checks whether its child has exited. Small enough
/// that a query that finishes promptly is not delayed, large enough that the
/// wait loop is not a busy spin.
const BOUNDED_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// A command builder that always captures output. Chain `arg`/`args`/
/// `current_dir`/`env`, then finish with [`run`](Cmd::run).
#[must_use = "a Cmd does nothing until `.run()` is called"]
pub struct Cmd {
    inner: Command,
    /// Human-readable `program arg arg ...`, used in error messages.
    label: String,
}

#[cfg(test)]
/// Which output stream produced a progress line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputStream {
    Stdout,
    Stderr,
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
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    /// Run to completion like [`run`](Cmd::run), but give up after `timeout`:
    /// the child is killed and an error returned. Both streams are drained on
    /// dedicated threads while the deadline is watched, so a child that fills a
    /// pipe buffer cannot deadlock the wait.
    ///
    /// For the terminal query, which a stuck or overloaded kitty would otherwise
    /// block without bound. Deliberately a separate method rather than a bound on
    /// [`run`](Cmd::run): agent Runs and `jj` operations are legitimately long.
    pub fn run_with_timeout(mut self, timeout: Duration) -> Result<Run> {
        let mut child = self
            .inner
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("running {}", self.label))?;
        let stdout = child.stdout.take().context("capturing command stdout")?;
        let stderr = child.stderr.take().context("capturing command stderr")?;
        let stdout_reader = spawn_pipe_reader(stdout);
        let stderr_reader = spawn_pipe_reader(stderr);

        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    reap_killed(&mut child, stdout_reader, stderr_reader);
                    return Err(anyhow!("{} did not finish within {timeout:?}", self.label));
                }
                Ok(None) => thread::sleep(BOUNDED_POLL_INTERVAL),
                Err(source) => {
                    reap_killed(&mut child, stdout_reader, stderr_reader);
                    return Err(anyhow!("waiting for {}: {source}", self.label));
                }
            }
        };
        let stdout = finish_pipe(stdout_reader, "stdout")?;
        let stderr = finish_pipe(stderr_reader, "stderr")?;
        Ok(Run {
            label: self.label,
            ok: status.success(),
            code: status.code(),
            stdout,
            stderr,
        })
    }

    #[cfg(test)]
    /// Run to completion while delivering complete output lines to `on_line`.
    /// Both streams remain captured in the returned [`Run`].
    pub(crate) fn run_streaming<F>(mut self, mut on_line: F) -> Result<Run>
    where
        F: FnMut(OutputStream, &str),
    {
        let mut child = self
            .inner
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("running {}", self.label))?;
        let stdout = child.stdout.take().context("capturing command stdout")?;
        let stderr = child.stderr.take().context("capturing command stderr")?;
        let (tx, rx) = mpsc::channel();
        let stdout_thread = spawn_output_reader(stdout, OutputStream::Stdout, tx.clone());
        let stderr_thread = spawn_output_reader(stderr, OutputStream::Stderr, tx);

        let mut stdout_text = String::new();
        let mut stderr_text = String::new();
        for (stream, line) in rx {
            match stream {
                OutputStream::Stdout => stdout_text.push_str(&line),
                OutputStream::Stderr => stderr_text.push_str(&line),
            }
            on_line(stream, line.trim_end_matches(['\r', '\n']));
        }
        stdout_thread
            .join()
            .map_err(|_| anyhow!("reading command stdout failed"))??;
        stderr_thread
            .join()
            .map_err(|_| anyhow!("reading command stderr failed"))??;
        let status = child
            .wait()
            .with_context(|| format!("waiting for {}", self.label))?;
        Ok(Run {
            label: self.label,
            ok: status.success(),
            code: status.code(),
            stdout: stdout_text,
            stderr: stderr_text,
        })
    }
}

/// Drain a child pipe to EOF on its own thread, as lossy UTF-8. Reading
/// concurrently with the wait is what keeps a full pipe buffer from wedging the
/// child.
fn spawn_pipe_reader<R: Read + Send + 'static>(mut pipe: R) -> thread::JoinHandle<Result<String>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    })
}

/// Collect one drained pipe, mapping a panicked reader to an error.
fn finish_pipe(reader: thread::JoinHandle<Result<String>>, name: &'static str) -> Result<String> {
    reader
        .join()
        .map_err(|_| anyhow!("reading command {name} failed"))?
}

/// Kill a child that overran its deadline and reap it.
///
/// The pipe readers are detached rather than joined: joining waits for every
/// holder of the child's stdout and stderr to close them, and a surviving
/// descendant could postpone that indefinitely - which would defeat the very
/// bound this exists to enforce. The threads end at EOF once the pipes close.
fn reap_killed(
    child: &mut Child,
    stdout_reader: thread::JoinHandle<Result<String>>,
    stderr_reader: thread::JoinHandle<Result<String>>,
) {
    let _ = child.kill();
    let _ = child.wait();
    drop(stdout_reader);
    drop(stderr_reader);
}

#[cfg(test)]
fn spawn_output_reader<R: std::io::Read + Send + 'static>(
    reader: R,
    stream: OutputStream,
    tx: mpsc::Sender<(OutputStream, String)>,
) -> thread::JoinHandle<Result<()>> {
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut line = Vec::new();
        loop {
            line.clear();
            let bytes = reader.read_until(b'\n', &mut line)?;
            if bytes == 0 {
                break;
            }
            let text = String::from_utf8_lossy(&line).into_owned();
            tx.send((stream, text))
                .map_err(|_| anyhow!("progress receiver closed"))?;
        }
        Ok(())
    })
}

/// A finished command's captured result (stdout/stderr are lossy UTF-8).
#[derive(Debug)]
pub struct Run {
    label: String,
    ok: bool,
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Run {
    /// Did the process exit zero?
    pub fn ok(&self) -> bool {
        self.ok
    }

    /// The process's exit code, when it exited normally. `None` when a signal
    /// killed it. Lets a caller tell a shell's "command not found" (127) from a
    /// command that ran and reported a failure.
    pub fn code(&self) -> Option<i32> {
        self.code
    }

    /// Captured stdout, whether or not the run succeeded.
    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    /// Captured stderr, whether or not the run succeeded. jj writes its status
    /// and error lines here, so a failed run can carry jj's own message. jjfx
    /// never derives a semantic from this English text - the typed jj access
    /// layer in `jj` classifies outcomes from machine-readable state instead.
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
impl Run {
    /// A canned run, for tests that classify captured jj output without spawning
    /// a subprocess - the stub jj the typed access layer's outcomes are exercised
    /// against.
    pub(crate) fn for_test(ok: bool, stdout: &str, stderr: &str) -> Self {
        Self {
            label: "jj (test)".to_string(),
            ok,
            code: ok.then_some(0),
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
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
    fn bounded_run_captures_a_prompt_command() {
        let run = cmd("printf")
            .arg("hello")
            .run_with_timeout(Duration::from_secs(5))
            .expect("printf finishes well within the bound");
        assert!(run.ok());
        assert_eq!(run.stdout(), "hello");
    }

    #[test]
    fn bounded_run_kills_a_child_that_overruns_its_deadline() {
        let started = Instant::now();
        let error = cmd("sleep")
            .arg("30")
            .run_with_timeout(Duration::from_millis(200))
            .expect_err("a child that overruns must be killed, not waited on");
        assert!(error.to_string().contains("did not finish"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the bound must return promptly, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn non_zero_exit_is_a_completed_run_not_an_error() {
        let run = cmd("false").run().expect("false spawns");
        assert!(!run.ok());
        assert!(run.stdout_ok().is_none());
    }

    #[test]
    fn streaming_captures_both_streams_and_reports_each_line() {
        let mut lines = Vec::new();
        let run = cmd("sh")
            .args([
                "-c",
                "printf 'out 1\\nout 2\\n'; printf 'err 1\\n' >&2; exit 7",
            ])
            .run_streaming(|stream, line| lines.push((stream, line.to_owned())))
            .expect("sh spawns");

        assert!(!run.ok());
        assert_eq!(run.stdout(), "out 1\nout 2\n");
        assert_eq!(run.stderr(), "err 1\n");
        assert!(lines.contains(&(OutputStream::Stdout, "out 1".to_owned())));
        assert!(lines.contains(&(OutputStream::Stdout, "out 2".to_owned())));
        assert!(lines.contains(&(OutputStream::Stderr, "err 1".to_owned())));
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
