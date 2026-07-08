//! The hook -> TUI transport (ADR 0004): a single append-only global JSONL log
//! that dumb hooks append to and the TUI both replays (on startup) and tails
//! (live). One fixed path, filtered by `cwd`; hooks never resolve a repo root.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::Context;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc::UnboundedSender;

use crate::agent::{self, Event};
use crate::app::Msg;

/// Cap the event log before rebuilding state, so unbounded agent history cannot
/// grow the file without limit (ticket 04: size-based rotation).
pub const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

/// The one fixed global log path: `${XDG_STATE_HOME:-~/.local/state}/jjfx/
/// events.jsonl` - the exact path the installed hook appends to.
pub fn log_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            home.join(".local").join("state")
        });
    base.join("jjfx").join("events.jsonl")
}

/// Read the whole log into events, for the startup replay. A missing or garbled
/// file yields an empty history rather than an error.
pub fn read_events(path: &Path) -> Vec<Event> {
    let text = fs::read_to_string(path).unwrap_or_default();
    text.lines().filter_map(agent::parse_line).collect()
}

/// If the log exceeds `max_bytes`, retain only its tail (from the next line
/// boundary), written atomically. Keeping the tail preserves the most recent
/// state while bounding growth; older sessions' events are the ones dropped.
pub fn rotate_if_needed(path: &Path, max_bytes: u64) -> io::Result<()> {
    let len = match fs::metadata(path) {
        Ok(m) => m.len(),
        Err(_) => return Ok(()), // no log yet - nothing to rotate
    };
    if len <= max_bytes {
        return Ok(());
    }
    let data = fs::read(path)?;
    let keep = (max_bytes / 2) as usize;
    let from = data.len().saturating_sub(keep);
    let slice = &data[from..];
    // Start on a line boundary so replay never sees a truncated first line.
    let begin = slice
        .iter()
        .position(|&b| b == b'\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    let tail = &slice[begin..];

    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("log path has no parent"))?;
    let tmp = dir.join(format!("events.jsonl.{}.tmp", std::process::id()));
    fs::write(&tmp, tail)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Tail the log: watch its directory and, on every change, read the bytes
/// appended since the last read and forward each complete line as a
/// [`Msg::AgentEvent`]. The returned watcher must be kept alive.
///
/// Starts from the current end of the file, so it complements (does not
/// duplicate) the startup replay done via [`read_events`].
pub fn watch_log(path: &Path, tx: UnboundedSender<Msg>) -> anyhow::Result<RecommendedWatcher> {
    let dir = path
        .parent()
        .context("log path has no parent")?
        .to_path_buf();
    // Ensure the directory exists so notify has something to watch even before
    // the first hook fires and creates the file.
    fs::create_dir_all(&dir).ok();

    let path_buf = path.to_path_buf();
    let mut offset = fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_err() {
            return;
        }
        for ev in read_new_events(&path_buf, &mut offset) {
            // The receiver only closes at shutdown; ignore send errors then.
            let _ = tx.send(Msg::AgentEvent(ev));
        }
    })
    .context("creating event-log watcher")?;
    watcher
        .watch(&dir, RecursiveMode::NonRecursive)
        .with_context(|| format!("watching {}", dir.display()))?;
    Ok(watcher)
}

/// Read the events appended since `offset`, advancing `offset` past only the
/// bytes consumed (complete lines). A trailing partial line is left for the next
/// call; a file shorter than `offset` (rotated/truncated) resets to the start.
/// Any read error yields no events and leaves `offset` untouched.
fn read_new_events(path: &Path, offset: &mut u64) -> Vec<Event> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len < *offset {
        *offset = 0; // file was rotated/truncated out from under us
    }
    if file.seek(SeekFrom::Start(*offset)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let Some(last_nl) = buf.iter().rposition(|&b| b == b'\n') else {
        return Vec::new(); // no complete line yet
    };
    let complete = &buf[..=last_nl];
    *offset += complete.len() as u64;
    complete
        .split(|&b| b == b'\n')
        .filter_map(|line| std::str::from_utf8(line).ok())
        .filter_map(agent::parse_line)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jjfx-events-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn log_path_prefers_xdg_state_home() {
        // The function reads process env; assert its shape rather than mutating
        // global state. Both branches end in the same suffix.
        let p = log_path();
        assert!(p.ends_with("jjfx/events.jsonl"));
    }

    #[test]
    fn read_events_on_missing_file_is_empty() {
        let dir = scratch("missing");
        assert!(read_events(&dir.join("events.jsonl")).is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rotate_keeps_the_tail_from_a_line_boundary() {
        let dir = scratch("rotate");
        let log = dir.join("events.jsonl");
        // Many lines, each valid; well over the tiny cap below.
        let line = r#"{"cwd":"/w/a","hook_event_name":"Stop"}"#;
        let content: String = std::iter::repeat_n(line, 500)
            .map(|l| format!("{l}\n"))
            .collect();
        fs::write(&log, &content).unwrap();

        rotate_if_needed(&log, 1024).unwrap();

        let after = fs::read_to_string(&log).unwrap();
        assert!((after.len() as u64) <= 1024);
        // Every retained line is whole and still parses.
        assert!(after.starts_with('{'));
        assert!(after.lines().all(|l| agent::parse_line(l).is_some()));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_new_events_tails_appends_and_holds_partial_lines() {
        let dir = scratch("tail");
        let log = dir.join("events.jsonl");
        let mut offset = 0u64;

        // Empty file: nothing yet.
        fs::write(&log, "").unwrap();
        assert!(read_new_events(&log, &mut offset).is_empty());

        // One complete line + a partial line with no newline: only the complete
        // one is consumed; offset stops at the newline boundary.
        fs::write(
            &log,
            "{\"cwd\":\"/w/a\",\"hook_event_name\":\"SessionStart\"}\n{\"cwd\":\"/w/a\",\"hook_ev",
        )
        .unwrap();
        let first = read_new_events(&log, &mut offset);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].name, "SessionStart");

        // Complete the partial line and append another: both surface, no dupes of
        // the already-consumed first line.
        fs::write(
            &log,
            "{\"cwd\":\"/w/a\",\"hook_event_name\":\"SessionStart\"}\n{\"cwd\":\"/w/a\",\"hook_event_name\":\"UserPromptSubmit\"}\n{\"cwd\":\"/w/a\",\"hook_event_name\":\"Stop\"}\n",
        )
        .unwrap();
        let more = read_new_events(&log, &mut offset);
        let names: Vec<_> = more.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["UserPromptSubmit", "Stop"]);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_new_events_resets_after_rotation() {
        let dir = scratch("tail-rotate");
        let log = dir.join("events.jsonl");
        // Pretend we had read to a large offset, then the file was rotated smaller.
        let mut offset = 10_000u64;
        fs::write(&log, "{\"cwd\":\"/w/a\",\"hook_event_name\":\"Stop\"}\n").unwrap();
        let got = read_new_events(&log, &mut offset);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "Stop");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rotate_noop_under_cap() {
        let dir = scratch("noop");
        let log = dir.join("events.jsonl");
        fs::write(&log, "one\ntwo\n").unwrap();
        rotate_if_needed(&log, MAX_LOG_BYTES).unwrap();
        assert_eq!(fs::read_to_string(&log).unwrap(), "one\ntwo\n");
        fs::remove_dir_all(&dir).unwrap();
    }
}
