//! The stuck-Run watchdog: a live Run that has made no progress within a
//! configured window.
//!
//! Stuck is derived, not persisted. Like occupancy (ADR-0011) it is a view of
//! live Run state - the Run's start, its Run log's last write, and its process -
//! so it never enters the Go-compatible Pool and Worker documents. The engine
//! decides it from facts it is handed; a frontend supplies the clock and the
//! configured window, exactly as it supplies the occupied set for Dispatch.

use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;

use crate::engine::WorkerStatus;

/// How long a Run may make no progress before the watchdog calls it stuck.
///
/// A window of zero disables the watchdog, which is how a frontend turns the
/// signal off without a second switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StuckWindow(Duration);

impl StuckWindow {
    /// The window that disables the watchdog: no Run is ever stuck.
    pub const DISABLED: Self = Self(Duration::ZERO);
    /// The window used when none is configured. Long enough that a slow model
    /// turn or a long tool call is not mistaken for a wedge, short enough that a
    /// genuinely blocked Run surfaces within the same working session.
    pub const DEFAULT: Self = Self(Duration::from_secs(30 * 60));

    /// Creates a window of `seconds`; zero disables the watchdog.
    pub const fn from_secs(seconds: u64) -> Self {
        Self(Duration::from_secs(seconds))
    }

    /// Returns the window as a plain duration.
    pub const fn as_duration(self) -> Duration {
        self.0
    }

    /// Reports whether the watchdog is watching at all.
    pub const fn is_enabled(self) -> bool {
        self.0.as_secs() > 0
    }
}

impl Default for StuckWindow {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The watchdog's verdict on one Run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunHealth {
    /// Progressing, finished, or not watched: nothing for the owner to do.
    #[default]
    Live,
    /// No progress within the window: the owner should look.
    Stuck,
}

impl RunHealth {
    /// Reports whether the watchdog has called this Run stuck.
    pub const fn is_stuck(self) -> bool {
        matches!(self, Self::Stuck)
    }
}

/// The live facts the watchdog judges one Run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunProgress {
    /// The Worker's recorded status. Only a busy Run can be stuck.
    pub status: WorkerStatus,
    /// When the Run started, when it is recorded and readable.
    pub started_at: Option<Timestamp>,
    /// When the Run's log was last written, when it can be read.
    pub log_activity_at: Option<Timestamp>,
    /// Whether the Run's recorded process is alive. A Run whose process is gone
    /// is reconciliation's problem - it becomes failed, not stuck.
    pub process_alive: bool,
}

impl RunProgress {
    /// The most recent sign of progress: the Run's start or its last log write.
    ///
    /// A Run that just started has nothing to be stuck on, and a log still being
    /// written is progress by definition, so the later of the two is the Run's
    /// last known progress. A Run with neither has no evidence either way and is
    /// never called stuck on a guess.
    pub fn last_progress_at(self) -> Option<Timestamp> {
        match (self.started_at, self.log_activity_at) {
            (Some(started), Some(activity)) => Some(started.max(activity)),
            (started, activity) => started.or(activity),
        }
    }
}

/// The watchdog's verdict for one Run.
pub fn health(progress: RunProgress, now: Timestamp, window: StuckWindow) -> RunHealth {
    if !window.is_enabled() || progress.status != WorkerStatus::Busy || !progress.process_alive {
        return RunHealth::Live;
    }
    let Some(last) = progress.last_progress_at() else {
        return RunHealth::Live;
    };
    if now.duration_since(last).as_secs_f64() > window.as_duration().as_secs_f64() {
        RunHealth::Stuck
    } else {
        RunHealth::Live
    }
}

/// The last write time of a Run's log, when it can be read.
///
/// A Run log is appended to as the agent works, so its modification time is a
/// cheap progress signal that needs no parsing - and it is the only signal a
/// Run whose Agent Runtime reports nothing else still leaves behind.
pub fn log_activity_at(log_file: Option<&Path>) -> Option<Timestamp> {
    let modified = std::fs::metadata(log_file?).ok()?.modified().ok()?;
    Timestamp::try_from(modified).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: &str) -> Timestamp {
        value.parse().expect("a test timestamp")
    }

    fn progress(
        status: WorkerStatus,
        started: Option<&str>,
        activity: Option<&str>,
        alive: bool,
    ) -> RunProgress {
        RunProgress {
            status,
            started_at: started.map(at),
            log_activity_at: activity.map(at),
            process_alive: alive,
        }
    }

    const WINDOW: StuckWindow = StuckWindow::from_secs(600);

    #[test]
    fn a_busy_run_with_a_stale_log_is_stuck() {
        let run = progress(
            WorkerStatus::Busy,
            Some("2026-01-01T00:00:00Z"),
            Some("2026-01-01T00:05:00Z"),
            true,
        );
        assert_eq!(
            health(run, at("2026-01-01T01:00:00Z"), WINDOW),
            RunHealth::Stuck
        );
    }

    #[test]
    fn a_log_written_inside_the_window_is_progress() {
        let run = progress(
            WorkerStatus::Busy,
            Some("2026-01-01T00:00:00Z"),
            Some("2026-01-01T00:59:00Z"),
            true,
        );
        assert_eq!(
            health(run, at("2026-01-01T01:00:00Z"), WINDOW),
            RunHealth::Live
        );
    }

    #[test]
    fn a_run_that_just_started_is_not_stuck() {
        let run = progress(
            WorkerStatus::Busy,
            Some("2026-01-01T00:59:30Z"),
            None,
            true,
        );
        assert_eq!(
            health(run, at("2026-01-01T01:00:00Z"), WINDOW),
            RunHealth::Live
        );
    }

    #[test]
    fn only_a_busy_live_run_can_be_stuck() {
        let stale = at("2026-01-01T00:00:00Z");
        let now = at("2026-01-01T01:00:00Z");
        for status in [
            WorkerStatus::Idle,
            WorkerStatus::Done,
            WorkerStatus::Failed,
        ] {
            let run = progress(status, Some("2026-01-01T00:00:00Z"), None, true);
            assert_eq!(health(run, now, WINDOW), RunHealth::Live, "{status:?}");
        }
        let dead = RunProgress {
            status: WorkerStatus::Busy,
            started_at: Some(stale),
            log_activity_at: None,
            process_alive: false,
        };
        assert_eq!(health(dead, now, WINDOW), RunHealth::Live);
    }

    #[test]
    fn a_disabled_window_never_flags_a_run() {
        let run = progress(
            WorkerStatus::Busy,
            Some("2026-01-01T00:00:00Z"),
            Some("2026-01-01T00:00:01Z"),
            true,
        );
        assert_eq!(
            health(run, at("2026-02-01T00:00:00Z"), StuckWindow::DISABLED),
            RunHealth::Live
        );
        assert!(!StuckWindow::DISABLED.is_enabled());
        assert!(StuckWindow::DEFAULT.is_enabled());
    }

    #[test]
    fn a_run_with_no_known_progress_is_never_stuck() {
        let run = progress(WorkerStatus::Busy, None, None, true);
        assert_eq!(
            health(run, at("2026-02-01T00:00:00Z"), WINDOW),
            RunHealth::Live
        );
    }

    #[test]
    fn a_log_activity_time_is_read_from_a_real_file() {
        let directory = std::env::temp_dir().join("jjfx-watchdog-test");
        std::fs::create_dir_all(&directory).expect("temp directory");
        let log = directory.join("run.log");
        std::fs::write(&log, "{\"type\":\"system\"}\n").expect("write log");
        let activity = log_activity_at(Some(&log)).expect("a log write time");
        assert!(activity <= Timestamp::now());
        assert_eq!(log_activity_at(Some(&directory.join("absent.log"))), None);
        assert_eq!(log_activity_at(None), None);
    }
}
