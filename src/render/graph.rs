//! Commit-graph rendering: the world DAG laid out like `jj log`, the per-commit
//! row styling, and the per-workspace strip shown in the detail view. Pure over
//! `graph::Graph` plus a width - no `App` dependency.

use std::collections::HashSet;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
// sapling-renderdag exports its library as plain `renderdag`.
use renderdag::{Ancestor, GraphRowRenderer, Renderer};

use crate::graph;
use crate::render::style::{dim_line, elide_right, freshness_style, now_millis, pane_border};

/// One commit row: `<prefix><glyph><change-id>[ @]  <summary> <bookmarks>`. The
/// change id is freshness-shaded (bold when on the highlighted chain); the
/// summary is budgeted to the remaining width so a long line never wraps and
/// corrupts the layout.
#[allow(clippy::too_many_arguments)]
fn commit_line(
    prefix: &str,
    glyph: &str,
    glyph_color: Color,
    node: &graph::Node,
    is_head: bool,
    selected: bool,
    now_ms: i64,
    width: u16,
) -> Line<'static> {
    let mut id_style = freshness_style(node.timestamp_ms, now_ms);
    if selected {
        id_style = id_style.add_modifier(Modifier::BOLD);
    }
    let mut spans = vec![
        Span::raw(prefix.to_string()),
        Span::styled(glyph.to_string(), Style::default().fg(glyph_color)),
        Span::styled(node.change_id.clone(), id_style),
    ];
    if is_head {
        spans.push(Span::styled(
            " @",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let bookmarks_w: usize = node.bookmarks.iter().map(|b| b.chars().count() + 1).sum();
    let used = prefix.chars().count()
        + glyph.chars().count()
        + node.change_id.chars().count()
        + if is_head { 2 } else { 0 }
        + 1;
    let budget = (width as usize)
        .saturating_sub(used + bookmarks_w + 1)
        .max(6);
    let summary_style = if selected {
        Style::default()
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        elide_right(&node.summary, budget),
        summary_style,
    ));
    for bm in &node.bookmarks {
        spans.push(Span::styled(
            format!(" {bm}"),
            Style::default().fg(Color::Magenta),
        ));
    }
    Line::from(spans)
}

/// The world view: the commit DAG laid out like `jj log` - every mutable
/// commit, each fragment's trunk branch point, and the trunk tip, history
/// below elided as `~`. Columns and edges come from sapling-renderdag (the
/// renderer jj's own CLI uses), so the shape matches `jj log`'s; the selected
/// workspace's chain is highlighted.
pub(crate) fn world_graph_lines(
    g: &graph::Graph,
    selected: Option<&str>,
    now_ms: i64,
    width: u16,
) -> Vec<Line<'static>> {
    // The message slot carries a sentinel so the graph prefix can be split
    // back out of the rendered row and restyled; the text spans are ours.
    const MARK: char = '\u{1}';
    let selected_ids: HashSet<&str> = selected
        .and_then(|w| g.chain(w))
        .map(|c| {
            c.commits
                .iter()
                .map(String::as_str)
                .chain([c.head.as_str()])
                .collect()
        })
        .unwrap_or_default();

    let mut renderer = GraphRowRenderer::new()
        .output()
        .with_min_row_height(1)
        .build_box_drawing();
    let mut lines = Vec::new();
    for row in graph::log_rows(g) {
        let Some(node) = g.nodes.get(&row.id) else {
            continue;
        };
        let is_sel = selected_ids.contains(row.id.as_str());
        let glyph = if !node.wc_of.is_empty() {
            "@"
        } else if node.immutable {
            "◆"
        } else {
            "○"
        };
        let parents = row
            .edges
            .iter()
            .map(|e| match e {
                graph::LogEdge::Direct(p) => Ancestor::Parent(p.clone()),
                graph::LogEdge::Elided(p) => Ancestor::Ancestor(p.clone()),
                graph::LogEdge::Missing => Ancestor::Anonymous,
            })
            .collect();
        let rendered =
            renderer.next_row(row.id.clone(), parents, glyph.to_string(), MARK.to_string());
        for text in rendered.lines() {
            match text.split_once(MARK) {
                Some((prefix, _)) => {
                    lines.push(world_row(prefix, glyph, node, is_sel, now_ms, width));
                }
                // A pure link/termination row (fork, merge, `~`): no commit text.
                None => lines.push(dim_line(text)),
            }
        }
    }
    lines
}

/// One commit row of the world graph: the graph prefix (edges dim, the node
/// glyph coloured), then change id, `name@` working-copy badges, summary, and
/// bookmarks - sized to the pane width.
fn world_row(
    prefix: &str,
    glyph: &str,
    node: &graph::Node,
    selected: bool,
    now_ms: i64,
    width: u16,
) -> Line<'static> {
    let glyph_color = if selected {
        Color::Cyan
    } else if !node.wc_of.is_empty() {
        Color::White
    } else if node.immutable {
        Color::DarkGray
    } else {
        Color::Gray
    };
    let edge_style = Style::default().add_modifier(Modifier::DIM);
    // The glyph is the one non-edge character in the prefix; split on it so
    // the edges around it stay dim while the node itself is coloured.
    let (before, after) = prefix.split_once(glyph).unwrap_or((prefix, ""));
    let mut spans = vec![
        Span::styled(before.to_string(), edge_style),
        Span::styled(glyph.to_string(), Style::default().fg(glyph_color)),
        Span::styled(after.to_string(), edge_style),
    ];

    let mut id_style = freshness_style(node.timestamp_ms, now_ms);
    if selected {
        id_style = id_style.add_modifier(Modifier::BOLD);
    }
    spans.push(Span::styled(node.change_id.clone(), id_style));
    for ws in &node.wc_of {
        spans.push(Span::styled(
            format!(" {ws}@"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let wc_w: usize = node.wc_of.iter().map(|w| w.chars().count() + 2).sum();
    let bookmarks_w: usize = node.bookmarks.iter().map(|b| b.chars().count() + 1).sum();
    let used = prefix.chars().count() + node.change_id.chars().count() + wc_w + 1;
    let budget = (width as usize)
        .saturating_sub(used + bookmarks_w + 1)
        .max(6);
    let summary_style = if selected {
        Style::default()
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        elide_right(&node.summary, budget),
        summary_style,
    ));
    for bm in &node.bookmarks {
        spans.push(Span::styled(
            format!(" {bm}"),
            Style::default().fg(Color::Magenta),
        ));
    }
    Line::from(spans)
}

/// The per-workspace strip in the detail view: the one child past `@` (if any),
/// the workspace's own commits with connectors, and the trunk commit it attaches
/// to. Always rendered as the highlighted chain (it is the workspace in view).
fn workspace_graph_lines(
    g: &graph::Graph,
    chain: &graph::Chain,
    now_ms: i64,
    width: u16,
) -> Vec<Line<'static>> {
    let w = width.saturating_sub(2);
    let mut lines = Vec::new();

    if let Some(cid) = &chain.child
        && let Some(node) = g.nodes.get(cid)
    {
        let mut l = commit_line("", "◆ ", Color::Yellow, node, false, true, now_ms, w);
        l.spans.push(Span::styled(
            "  +1",
            Style::default().add_modifier(Modifier::DIM),
        ));
        lines.push(l);
        lines.push(dim_line("│"));
    }

    if chain.commits.is_empty() {
        lines.push(dim_line(" on trunk (clean)"));
    }
    for id in &chain.commits {
        if let Some(node) = g.nodes.get(id) {
            let is_head = id == &chain.head;
            let glyph = if is_head { "● " } else { "○ " };
            lines.push(commit_line(
                "",
                glyph,
                Color::Cyan,
                node,
                is_head,
                true,
                now_ms,
                w,
            ));
            lines.push(dim_line("│"));
        }
    }

    match chain.base.as_ref().and_then(|b| g.nodes.get(b)) {
        Some(node) => lines.push(commit_line(
            "",
            "● ",
            Color::DarkGray,
            node,
            false,
            false,
            now_ms,
            w,
        )),
        // No trunk anchor loaded: drop the trailing connector so it doesn't dangle.
        None => {
            lines.pop();
        }
    }
    lines
}

/// The per-workspace graph strip in the detail view.
pub(crate) fn render_graph_pane(
    frame: &mut Frame,
    graph: Option<&graph::Graph>,
    ws: &str,
    area: Rect,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border(false))
        .title(" graph ");
    let lines: Vec<Line> = match graph {
        Some(g) => match g.chain(ws) {
            Some(chain) => workspace_graph_lines(g, chain, now_millis(), area.width),
            None => vec![dim_line(" (no chain)")],
        },
        None => vec![dim_line(" loading…")],
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}
