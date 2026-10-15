//! The hook -> TUI transport (ADR 0004): a single append-only global JSONL log
//! that dumb hooks append to and the TUI both replays (on startup) and tails
//! (live). One fixed path, filtered by `cwd`; hooks never resolve a repo root.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc::UnboundedSender;

use crate::agent::{self, Event};
use crate::app::Msg;

/// Cap the event log before rebuilding state, so unbounded agent history cannot
/// grow the file without limit (ticket 04: size-based rotation).
pub const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

/// The one fixed global state directory jjfx owns:
/// `${XDG_STATE_HOME:-~/.local/state}/jjfx`.
pub fn state_dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            home.join(".local").join("state")
        });
    base.join("jjfx")
}

/// The one fixed global log path: `${XDG_STATE_HOME:-~/.local/state}/jjfx/
/// events.jsonl` - the exact path the installed hook appends to.
pub fn log_path() -> PathBuf {
    state_dir().join("events.jsonl")
}

/// Where the TUI appends full setup/tab failure text, so a failure the footer
/// truncated stays inspectable after the fact.
pub fn setup_error_log_path() -> PathBuf {
    state_dir().join("setup-errors.log")
}

/// Append a timestamped failure block to `path`, best-effort. A missing
/// directory or write error is ignored: diagnostics must never break the TUI.
pub fn append_failure(path: &Path, context: &str, message: &str) {
    let Some(dir) = path.parent() else {
        return;
    };
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let block = format!("[{}] {context}\n{message}\n\n", jiff::Timestamp::now());
    let _ = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(block.as_bytes()));
}

/// Replay the log: the events it holds, plus the byte offset just past its last
/// complete line.
///
/// The offset is what [`watch_log`] must start from. Seeding the tailer from the
/// file length instead would drop any line appended between the replay and the
/// watcher's first read; seeding it from the replay's own consumed length closes
/// that window without replaying anything twice.
///
/// A missing or garbled file yields an empty history rather than an error.
pub fn replay(path: &Path) -> (Vec<Event>, u64) {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(_) => return (Vec::new(), 0),
    };
    let consumed = data
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map_or(0, |index| index + 1);
    (parse_lines(&data[..consumed]), consumed as u64)
}

/// Parse newline-delimited JSONL bytes into events.
///
/// Every line is decoded lossily, so one invalid byte - a rotation slice cut
/// mid-character, a torn write - costs at most that line instead of the whole
/// history. Lines that are not events are skipped.
fn parse_lines(bytes: &[u8]) -> Vec<Event> {
    bytes
        .split(|&byte| byte == b'\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| agent::parse_line(&String::from_utf8_lossy(line)))
        .collect()
}

/// If the log exceeds `max_bytes`, retain only its tail (from the next line
/// boundary), written atomically. Keeping the tail preserves the most recent
/// state while bounding growth; older sessions' events are the ones dropped.
///
/// Returns the new length when it actually rotated, so a live tailer can reseed
/// its offset past the retained tail it has already emitted instead of
/// re-emitting it as duplicates.
pub fn rotate_if_needed(path: &Path, max_bytes: u64) -> io::Result<Option<u64>> {
    let len = match fs::metadata(path) {
        Ok(m) => m.len(),
        Err(_) => return Ok(None), // no log yet - nothing to rotate
    };
    if len <= max_bytes {
        return Ok(None);
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
    Ok(Some(tail.len() as u64))
}

/// Tail the log: watch its directory and, on every change, read the bytes
/// appended since the last read and forward each complete line as a
/// [`Msg::AgentEvent`]. The returned watcher must be kept alive.
///
/// `offset` must be the length consumed by the startup [`replay`], so a line
/// appended between the replay and this call is neither lost nor replayed twice.
/// The log is also rotated here, on every change, so a long-lived TUI bounds
/// `events.jsonl` rather than only bounding it at startup.
pub fn watch_log(
    path: &Path,
    tx: UnboundedSender<Msg>,
    offset: u64,
) -> anyhow::Result<RecommendedWatcher> {
    let dir = path
        .parent()
        .context("log path has no parent")?
        .to_path_buf();
    // Ensure the directory exists so notify has something to watch even before
    // the first hook fires and creates the file.
    fs::create_dir_all(&dir).ok();

    let path_buf = path.to_path_buf();
    let mut offset = offset;

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_err() {
            return;
        }
        for ev in read_new_events(&path_buf, &mut offset) {
            // The receiver only closes at shutdown; ignore send errors then.
            let _ = tx.send(Msg::AgentEvent(ev));
        }
        // Rotate after reading, so the retained tail is exactly the history just
        // emitted: reseeding past it keeps it from being re-emitted.
        if let Ok(Some(rotated)) = rotate_if_needed(&path_buf, MAX_LOG_BYTES) {
            offset = rotated;
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
/// call.
///
/// A file shorter than `offset` was rotated or truncated out from under us. The
/// retained tail is already-emitted history, so skip to the new end rather than
/// replaying it as duplicates.
/// Any read error yields no events and leaves `offset` untouched.
fn read_new_events(path: &Path, offset: &mut u64) -> Vec<Event> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len < *offset {
        *offset = len;
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
    parse_lines(complete)
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
    fn replay_on_missing_file_is_empty() {
        let dir = scratch("missing");
        let (events, offset) = replay(&dir.join("events.jsonl"));
        assert!(events.is_empty());
        assert_eq!(offset, 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn append_failure_writes_a_timestamped_block_and_appends() {
        let dir = scratch("failure");
        let path = dir.join("nested").join("setup-errors.log");
        append_failure(&path, "create workspace 'feat' (Create)", "boom");
        append_failure(&path, "open workspace 'feat'", "kitty unavailable");
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("create workspace 'feat' (Create)\nboom"),
            "{text}"
        );
        assert!(
            text.contains("open workspace 'feat'\nkitty unavailable"),
            "{text}"
        );
        assert!(
            text.ends_with("\n\n"),
            "blocks are blank-line separated: {text:?}"
        );
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
    fn read_new_events_skips_a_rotated_shrink_without_re_emitting() {
        let dir = scratch("tail-rotate");
        let log = dir.join("events.jsonl");
        let line = "{\"cwd\":\"/w/a\",\"hook_event_name\":\"Stop\"}\n";
        // Pretend we had read to a large offset, then the file was rotated
        // smaller by another instance. The retained tail is already-emitted
        // history: replaying it would duplicate every event in it.
        let mut offset = 10_000u64;
        fs::write(&log, line).unwrap();
        let got = read_new_events(&log, &mut offset);
        assert!(
            got.is_empty(),
            "a rotated shrink must not re-emit the retained tail"
        );
        assert_eq!(offset, line.len() as u64, "offset reseeds to the new end");

        // A later append is still seen.
        let mut file = OpenOptions::new().append(true).open(&log).unwrap();
        file.write_all(line.as_bytes()).unwrap();
        let later = read_new_events(&log, &mut offset);
        assert_eq!(later.len(), 1);
        assert_eq!(later[0].name, "Stop");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn replay_tolerates_invalid_utf8_in_one_line() {
        let dir = scratch("invalid-utf8");
        let log = dir.join("events.jsonl");
        // A rotation slice cut mid-character leaves a torn first line that is
        // neither valid UTF-8 nor valid JSON. It must cost that line only - not
        // the whole history, which is what a whole-file `read_to_string` did.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"{\"cwd\":\"/w/a\",\"hook_event_name\":\"SessionStart\"}\n");
        bytes.extend_from_slice(b"\xe2\x82\"},\"hook_event_name\":\"Stop\"}\n");
        bytes.extend_from_slice(b"{\"cwd\":\"/w/a\",\"hook_event_name\":\"Stop\"}\n");
        fs::write(&log, &bytes).unwrap();

        let (events, offset) = replay(&log);
        let names: Vec<_> = events.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["SessionStart", "Stop"]);
        assert_eq!(offset, bytes.len() as u64);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn replay_offset_hands_appends_to_the_tailer() {
        let dir = scratch("replay-offset");
        let log = dir.join("events.jsonl");
        let first = "{\"cwd\":\"/w/a\",\"hook_event_name\":\"SessionStart\"}\n";
        // A complete line plus a torn one, as a hook mid-write leaves it.
        fs::write(&log, format!("{first}{{\"cwd\":\"/w/a\",\"hook_ev")).unwrap();

        let (events, replay_offset) = replay(&log);
        assert_eq!(events.len(), 1);
        assert_eq!(replay_offset, first.len() as u64);

        // The torn line completes after the replay. A tailer seeded from the
        // file length would have started past it and dropped the event.
        fs::write(
            &log,
            format!("{first}{{\"cwd\":\"/w/a\",\"hook_event_name\":\"UserPromptSubmit\"}}\n"),
        )
        .unwrap();
        let mut offset = replay_offset;
        let tailed = read_new_events(&log, &mut offset);
        let names: Vec<_> = tailed.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["UserPromptSubmit"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tailer_rotation_bounds_the_log_without_re_emitting() {
        let dir = scratch("tail-rotate-live");
        let log = dir.join("events.jsonl");
        let line = "{\"cwd\":\"/w/a\",\"hook_event_name\":\"Stop\"}\n";
        let mut offset = 0u64;

        // One append well past the cap, as a long-lived TUI would accumulate.
        fs::write(&log, line.repeat(500)).unwrap();
        let first = read_new_events(&log, &mut offset);
        assert_eq!(first.len(), 500);

        // The tailer rotates after reading and reseeds past the retained tail.
        let rotated = rotate_if_needed(&log, 1024)
            .unwrap()
            .expect("a log over the cap rotates");
        offset = rotated;
        let bounded = fs::metadata(&log).unwrap().len();
        assert!(bounded <= 1024, "log stays under the cap: {bounded}");

        // Nothing is re-emitted from the retained tail.
        assert!(read_new_events(&log, &mut offset).is_empty());

        // And a later append is still seen.
        let mut file = OpenOptions::new().append(true).open(&log).unwrap();
        file.write_all(line.as_bytes()).unwrap();
        assert_eq!(read_new_events(&log, &mut offset).len(), 1);
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

    /// Append the exact single-line JSON a dumb hook would, the way the installed
    /// `printf '%s\n' "$(cat)" >> events.jsonl` command does. This is the fake
    /// hook emitter: no hook process, just the bytes it would write.
    fn emit(file: &mut File, line: &str) {
        file.write_all(line.as_bytes()).unwrap();
        file.write_all(b"\n").unwrap();
    }

    #[test]
    fn a_fake_hook_emitter_drives_the_lifecycle_and_attention_through_the_log() {
        use crate::agent::{AgentKind, AgentState, AgentStates};
        use crate::attention::{self, Attention};
        use crate::engine::RunHealth;
        use crate::work::WorkState;

        let dir = scratch("fake-emitter");
        let log = dir.join("events.jsonl");
        let workspace = dir.join("ws");
        fs::create_dir_all(&workspace).unwrap();
        let cwd = workspace.to_string_lossy();

        let mut file = File::create(&log).unwrap();
        // A Claude session that starts, works, asks for input, resumes, then
        // ends the turn on a rate-limit wall - with subagent/batch chatter mixed
        // in to prove it never regresses the turn.
        for line in [
            format!(
                r#"{{"session_id":"s1","transcript_path":"/u/.claude/projects/x/s.jsonl","cwd":"{cwd}","hook_event_name":"SessionStart"}}"#
            ),
            format!(
                r#"{{"session_id":"s1","cwd":"{cwd}","hook_event_name":"UserPromptSubmit","prompt":"go"}}"#
            ),
            format!(
                r#"{{"session_id":"s1","cwd":"{cwd}","hook_event_name":"SubagentStart","agent_type":"Explore"}}"#
            ),
            format!(r#"{{"session_id":"s1","cwd":"{cwd}","hook_event_name":"PostToolBatch"}}"#),
            format!(
                r#"{{"session_id":"s1","cwd":"{cwd}","hook_event_name":"Notification","notification_type":"agent_needs_input","message":"Claude needs your input"}}"#
            ),
        ] {
            emit(&mut file, &line);
        }
        // The blocked agent sorts to the top of the list as needs-you.
        let (events, _) = replay(&log);
        let states = AgentStates::replay(events);
        let agent = states.agent_for(&workspace);
        assert_eq!(agent.state, AgentState::NeedsAttention);
        assert_eq!(agent.kind, AgentKind::Claude);
        assert_eq!(
            attention::derive(agent.state, WorkState::Clean, RunHealth::Live),
            Attention::NeedsYou
        );

        // The approved tool resuming clears needs-attention back to Working, and
        // the turn then ends on a rate-limit StopFailure: a distinct, visible
        // quota block rather than a plain finished turn.
        for line in [
            format!(r#"{{"session_id":"s1","cwd":"{cwd}","hook_event_name":"PostToolUse"}}"#),
            format!(
                r#"{{"session_id":"s1","cwd":"{cwd}","hook_event_name":"StopFailure","error":"rate_limit","error_details":"429 Too Many Requests"}}"#
            ),
        ] {
            emit(&mut file, &line);
        }
        let (events, _) = replay(&log);
        let states = AgentStates::replay(events);
        let agent = states.agent_for(&workspace);
        assert_eq!(agent.state, AgentState::QuotaBlocked);
        assert_eq!(
            attention::derive(agent.state, WorkState::Clean, RunHealth::Live),
            Attention::NeedsYou
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_fake_emitter_billing_failure_also_blocks_the_run() {
        use crate::agent::{AgentState, AgentStates};

        let dir = scratch("fake-emitter-billing");
        let log = dir.join("events.jsonl");
        let workspace = dir.join("ws");
        fs::create_dir_all(&workspace).unwrap();
        let cwd = workspace.to_string_lossy();

        let mut file = File::create(&log).unwrap();
        for line in [
            format!(r#"{{"cwd":"{cwd}","hook_event_name":"SessionStart"}}"#),
            format!(r#"{{"cwd":"{cwd}","hook_event_name":"UserPromptSubmit"}}"#),
            format!(r#"{{"cwd":"{cwd}","hook_event_name":"StopFailure","error":"billing_error"}}"#),
        ] {
            emit(&mut file, &line);
        }
        assert_eq!(
            AgentStates::replay(replay(&log).0)
                .agent_for(&workspace)
                .state,
            AgentState::QuotaBlocked
        );

        fs::remove_dir_all(&dir).unwrap();
    }
}
