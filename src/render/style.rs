//! Shared rendering vocabulary: pane borders, the colour palette, agent glyphs
//! and animations, text elision, and the date/freshness helpers. Every renderer
//! draws its styling from here, so no renderer depends on `App`.

use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::agent::{self, AgentKind, AgentState};
use crate::attention::Attention;
use crate::forge;
use crate::work::WorkState;

/// Bright border when a pane has focus, dim otherwise. Shared by the diff
/// viewer's panes and the graph views.
pub(crate) fn pane_border(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::White)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// Milliseconds since the epoch, for freshness shading. Zero if the clock is
/// before the epoch (impossible in practice) - a render helper must not panic.
pub(crate) fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Freshness shading for a commit's change id: recently-moved commits read
/// brightest and fade to dim with age (ticket 11's optional freshness cue).
pub(crate) fn freshness_style(timestamp_ms: i64, now_ms: i64) -> Style {
    const HOUR: i64 = 3_600_000;
    const DAY: i64 = 24 * HOUR;
    const WEEK: i64 = 7 * DAY;
    let age = now_ms.saturating_sub(timestamp_ms);
    if age < 2 * HOUR {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else if age < WEEK {
        Style::default().fg(Color::Gray)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// A single dim connector/spacer line.
pub(crate) fn dim_line(s: &str) -> Line<'static> {
    Line::from(Span::styled(
        s.to_string(),
        Style::default().add_modifier(Modifier::DIM),
    ))
}

/// Truncate to `max` columns keeping the head, with a trailing ellipsis.
pub(crate) fn elide_right(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

/// Colour cue for the Attention badge - the primary triage signal.
pub(crate) fn attention_color(att: Attention) -> Color {
    match att {
        Attention::NeedsYou => Color::Red,
        Attention::Working => Color::Green,
        Attention::ReadyToForge => Color::Cyan,
        Attention::Idle => Color::DarkGray,
    }
}

/// The agent column's label: which agent lives here while a
/// session is live, `-` otherwise. The *state* is carried by the glyph and the
/// Attention grouping, so the label is free to name the agent instead.
pub(crate) fn agent_label(agent: agent::Agent) -> &'static str {
    match agent.state {
        AgentState::Absent | AgentState::Ended => "-",
        _ => agent.kind.label(),
    }
}

/// Colour cue for the agent cell - drawing the eye to what is live or blocked.
/// A working agent wears its brand colour, matching the animated glyph.
pub(crate) fn agent_color(agent: agent::Agent) -> Color {
    match agent.state {
        AgentState::Absent | AgentState::Ended => Color::DarkGray,
        AgentState::Working => brand_color(agent.kind),
        AgentState::Waiting => Color::Yellow,
        AgentState::NeedsAttention => Color::Red,
    }
}

/// The "claude is working" animation frames, straight from jj-wsx: petal glyphs
/// bounced back and forth (0 1 2 3 4 5 4 3 2 1 0 ...), one step per tick.
pub(crate) const CLAUDE_FRAMES: [char; 6] = ['❀', '✼', '✴', '✳', '✛', '•'];

/// Codex's working animation: a hexagon filling slice by slice, then
/// restarting (nerd font `md-hexagon_slice_1..6` - needs a nerd-font-patched
/// terminal font, which the kitty setup jjfx drives already assumes).
pub(crate) const CODEX_FRAMES: [char; 6] = [
    '\u{f0ac3}', // 󰫃 hexagon_slice_1
    '\u{f0ac4}', // 󰫄 hexagon_slice_2
    '\u{f0ac5}', // 󰫅 hexagon_slice_3
    '\u{f0ac6}', // 󰫆 hexagon_slice_4
    '\u{f0ac7}', // 󰫇 hexagon_slice_5
    '\u{f0ac8}', // 󰫈 hexagon_slice_6
];

/// Pi's working animation pulses from a dim dot to a filled center and back.
pub(crate) const PI_FRAMES: [char; 6] = ['·', '∙', '•', '●', '•', '∙'];

/// Claude's spinner orange - the colour jj-wsx gave the working animation.
pub(crate) const CLAUDE_ORANGE: Color = Color::Rgb(255, 149, 0);

/// Codex's cyan.
pub(crate) const CODEX_CYAN: Color = Color::Rgb(34, 211, 238);

/// Pi's violet identity.
pub(crate) const PI_VIOLET: Color = Color::Rgb(168, 85, 247);
/// OpenCode's green identity.
pub(crate) const OPENCODE_GREEN: Color = Color::Rgb(74, 222, 128);

/// Each known agent's signature colour, worn while working. Unknown identity
/// stays neutral instead of borrowing another provider's styling.
pub(crate) fn brand_color(kind: AgentKind) -> Color {
    match kind {
        AgentKind::Claude => CLAUDE_ORANGE,
        AgentKind::Codex => CODEX_CYAN,
        AgentKind::Pi => PI_VIOLET,
        AgentKind::OpenCode => OPENCODE_GREEN,
        AgentKind::Unknown => Color::DarkGray,
    }
}

/// The provider-specific working frame for this animation tick.
pub(crate) fn working_frame(kind: AgentKind, tick: u64) -> char {
    match kind {
        AgentKind::Claude => {
            let len = CLAUDE_FRAMES.len() as u64;
            let cycle = (len - 1) * 2;
            let pos = tick % cycle;
            let idx = if pos < len { pos } else { cycle - pos };
            CLAUDE_FRAMES[idx as usize]
        }
        AgentKind::Codex => CODEX_FRAMES[(tick % CODEX_FRAMES.len() as u64) as usize],
        AgentKind::Pi => PI_FRAMES[(tick % PI_FRAMES.len() as u64) as usize],
        AgentKind::OpenCode => CODEX_FRAMES[(tick % CODEX_FRAMES.len() as u64) as usize],
        AgentKind::Unknown => '?',
    }
}

/// The one-glyph agent status ahead of the label: the agent's own working
/// animation in its brand colour, its own static mark while it waits on the
/// human (yellow; red when blocked on a permission), and a dim dot when there
/// is no live session.
pub(crate) fn agent_glyph(agent: agent::Agent, tick: u64) -> Span<'static> {
    let (ch, color) = match agent.state {
        AgentState::Working => (working_frame(agent.kind, tick), brand_color(agent.kind)),
        AgentState::Waiting => (paused_glyph(agent.kind), Color::Yellow),
        AgentState::NeedsAttention => (paused_glyph(agent.kind), Color::Red),
        AgentState::Absent | AgentState::Ended => ('·', Color::DarkGray),
    };
    Span::styled(format!("{ch} "), Style::default().fg(color))
}

/// The static "session present, not working" mark. State still speaks through
/// yellow waiting or red attention colour; unknown identity remains neutral.
pub(crate) fn paused_glyph(kind: AgentKind) -> char {
    match kind {
        AgentKind::Claude => '✻',
        AgentKind::Codex => '\u{f02d9}', // 󰋙 hexagon_outline
        AgentKind::Pi => 'π',
        AgentKind::OpenCode => 'O',
        AgentKind::Unknown => '?',
    }
}

/// The compact forge pipeline for a row: a `⚒` sigil then one `letter+glyph` per
/// step (`f w p r`), each coloured by its live status.
pub(crate) fn forge_spans(progress: &forge::Progress) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled("⚒ ", Style::default().fg(Color::Magenta))];
    for (step, status) in progress.steps() {
        spans.push(Span::styled(
            format!("{}{} ", forge_step_letter(step), forge_glyph(status)),
            Style::default().fg(forge_color(status)),
        ));
    }
    spans
}

fn forge_step_letter(step: forge::Step) -> char {
    match step {
        forge::Step::Fetch => 'f',
        forge::Step::Weld => 'w',
        forge::Step::Push => 'p',
        forge::Step::PullRequest => 'r',
    }
}

/// Glyph for a forge step's status: pending, running, done, or skipped.
fn forge_glyph(status: forge::Status) -> char {
    match status {
        forge::Status::Pending => '·',
        forge::Status::Running => '…',
        forge::Status::Ok => '✓',
        forge::Status::Skipped => '~',
    }
}

/// Colour for a forge step's status.
fn forge_color(status: forge::Status) -> Color {
    match status {
        forge::Status::Pending => Color::DarkGray,
        forge::Status::Running => Color::Cyan,
        forge::Status::Ok => Color::Green,
        forge::Status::Skipped => Color::Yellow,
    }
}

/// Colour cue for the behind-trunk count: dim when close, yellow once far enough
/// behind that `tidyws` (or a weld) is worth running.
pub(crate) fn behind_color(behind: u32) -> Color {
    if behind >= 5 {
        Color::Yellow
    } else {
        Color::DarkGray
    }
}

/// Colour cue for a work state - progress toward merge, plus review verdict.
pub(crate) fn work_color(state: WorkState) -> Color {
    use crate::work::ReviewVerdict;
    match state {
        WorkState::Unknown => Color::DarkGray,
        WorkState::Clean => Color::DarkGray,
        WorkState::Dirty { .. } => Color::Yellow,
        WorkState::Pushed => Color::Cyan,
        WorkState::PrOpen {
            verdict: ReviewVerdict::ChangesRequested,
            ..
        } => Color::Red,
        WorkState::PrOpen {
            verdict: ReviewVerdict::Approved,
            ..
        } => Color::Green,
        WorkState::PrOpen { .. } => Color::Cyan,
        WorkState::Merged => Color::Magenta,
    }
}

/// A compact elapsed-time label ("5s", "3m", "2h", "1d") from an RFC 3339
/// timestamp; falls back to the raw string when it cannot be parsed.
pub(crate) fn elapsed_label(timestamp: &str) -> String {
    let Some(started) = parse_rfc3339_millis(timestamp) else {
        return timestamp.to_string();
    };
    let elapsed = now_millis().saturating_sub(started) / 1_000;
    if elapsed < 60 {
        format!("{elapsed}s")
    } else if elapsed < 3_600 {
        format!("{}m", elapsed / 60)
    } else if elapsed < 86_400 {
        format!("{}h", elapsed / 3_600)
    } else {
        format!("{}d", elapsed / 86_400)
    }
}

fn parse_rfc3339_millis(value: &str) -> Option<i64> {
    let (date, time) = value.strip_suffix('Z')?.split_once('T')?;
    let mut date_parts = date.split('-');
    let year = date_parts.next()?.parse::<i64>().ok()?;
    let month = date_parts.next()?.parse::<i64>().ok()?;
    let day = date_parts.next()?.parse::<i64>().ok()?;
    let mut time_parts = time.split(':');
    let hour = time_parts.next()?.parse::<i64>().ok()?;
    let minute = time_parts.next()?.parse::<i64>().ok()?;
    let seconds = time_parts.next()?;
    let (second, fraction) = seconds
        .split_once('.')
        .map_or((seconds, "0"), |parts| parts);
    let second = second.parse::<i64>().ok()?;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let fraction = fraction.chars().take(3).collect::<String>();
    let millis = if fraction.is_empty() {
        0
    } else {
        let digits = fraction.parse::<i64>().ok()?;
        digits * 10_i64.pow(3 - fraction.len() as u32)
    };
    let days = days_from_civil(year, month, day)?;
    Some(
        days.saturating_mul(86_400_000)
            .saturating_add(hour.saturating_mul(3_600_000))
            .saturating_add(minute.saturating_mul(60_000))
            .saturating_add(second.saturating_mul(1_000))
            .saturating_add(millis),
    )
}

fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}
