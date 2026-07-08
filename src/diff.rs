//! Diff detail for a workspace: read the git-format patch from the mainline base
//! to a workspace's `@`, parse it into per-file entries, and highlight each file
//! in-process with `syntect` (ADR 0007 - replacing the original `bat` pipe). The
//! `+`/`-` gutters are preserved as coloured markers; the code content is
//! highlighted by the file's language, degrading to plain text for unknown types.

use std::path::Path;
use std::sync::LazyLock;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

use crate::cmd::cmd;
use crate::work::TRUNK_BASE;

/// Above this many lines a single file's diff is rendered plain rather than
/// syntect-highlighted, so a pathologically large patch never hitches the render
/// loop. The `+`/`-` gutters stay coloured; only the content highlight is dropped.
const MAX_HIGHLIGHT_LINES: usize = 4000;

/// The syntax and theme sets are parsed once (a few ms) and shared for the
/// process lifetime; only the cheap per-file `HighlightLines` state is rebuilt.
static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

/// One line of a file's diff, classified by its git prefix. `text` is the content
/// with the one-char prefix stripped (for hunk/meta lines it is the whole line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// Unchanged context (` ` prefix).
    Context,
    /// An inserted line (`+` prefix).
    Added,
    /// A removed line (`-` prefix).
    Removed,
    /// A hunk header (`@@ ... @@`).
    Hunk,
    /// A non-patch note (e.g. a binary-file marker).
    Meta,
}

/// One changed file: its path (new side), insertion/deletion counts, and lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub added: u32,
    pub removed: u32,
    pub lines: Vec<DiffLine>,
}

/// Read and parse the diff from the mainline base to `<ws>@`. Returns an empty
/// vec on any jj failure or when the workspace matches trunk. Blocking - call it
/// from `spawn_blocking`, never the render task.
pub fn load(repo_root: &Path, ws: &str) -> Vec<FileDiff> {
    match jj_read(
        repo_root,
        &[
            "diff",
            "--from",
            TRUNK_BASE,
            "--to",
            &format!("{ws}@"),
            "--git",
        ],
    ) {
        Some(out) => parse(&out),
        None => Vec::new(),
    }
}

/// Parse git-format diff text into per-file entries. Anything before the first
/// `diff --git` header is ignored.
pub fn parse(diff: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("diff --git ") {
            files.push(FileDiff {
                path: header_path(path),
                added: 0,
                removed: 0,
                lines: Vec::new(),
            });
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue; // preamble before the first file header
        };

        // File-header noise carrying no patch content.
        if line.starts_with("index ")
            || line.starts_with("old mode ")
            || line.starts_with("new mode ")
            || line.starts_with("deleted file mode ")
            || line.starts_with("new file mode ")
            || line.starts_with("similarity index ")
            || line.starts_with("rename from ")
            || line.starts_with("rename to ")
            || line.starts_with("copy from ")
            || line.starts_with("copy to ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
        {
            continue;
        }

        if line.starts_with("@@") {
            file.lines.push(DiffLine {
                kind: LineKind::Hunk,
                text: line.to_string(),
            });
        } else if let Some(rest) = line.strip_prefix('+') {
            file.added += 1;
            file.lines.push(DiffLine {
                kind: LineKind::Added,
                text: rest.to_string(),
            });
        } else if let Some(rest) = line.strip_prefix('-') {
            file.removed += 1;
            file.lines.push(DiffLine {
                kind: LineKind::Removed,
                text: rest.to_string(),
            });
        } else if let Some(rest) = line.strip_prefix(' ') {
            file.lines.push(DiffLine {
                kind: LineKind::Context,
                text: rest.to_string(),
            });
        } else if line.starts_with("Binary files ") || line.starts_with("\\ No newline") {
            file.lines.push(DiffLine {
                kind: LineKind::Meta,
                text: line.to_string(),
            });
        } else if line.is_empty() {
            // A blank context line: the leading space is elided by some emitters.
            file.lines.push(DiffLine {
                kind: LineKind::Context,
                text: String::new(),
            });
        }
    }
    files
}

/// The new-side path from a `diff --git a/X b/Y` header. Falls back to the old
/// side (deletions may repeat the same path) or the raw header.
fn header_path(rest: &str) -> String {
    // `rest` is `a/<old> b/<new>`; the halves are space-separated. Paths with
    // spaces are rare in this repo - take the `b/` side after the last " b/".
    if let Some((_, new)) = rest.rsplit_once(" b/") {
        return new.to_string();
    }
    rest.strip_prefix("a/").unwrap_or(rest).to_string()
}

/// A resumable, lazy highlighter for one file's diff. syntect's highlighter is
/// stateful and must be fed lines top-down, so to show line N correctly lines
/// `0..N` must already be highlighted - but no further. [`ensure`](Self::ensure)
/// advances the highlight only as far as the viewport needs, so switching files
/// or opening a large diff never pays to highlight the whole thing up front.
pub struct FileHighlighter {
    /// `None` when the file has no known syntax or is too large - lines then
    /// render as plain content (the `+`/`-` gutters are still coloured).
    hl: Option<HighlightLines<'static>>,
    /// Highlighted lines produced so far, one per source diff line, in order.
    lines: Vec<Line<'static>>,
    /// Index of the next source line to highlight (`== lines.len()`).
    next: usize,
}

impl FileHighlighter {
    /// Prepare a highlighter for `file`, highlighting nothing yet.
    pub fn new(file: &FileDiff) -> Self {
        let ps = &*SYNTAX_SET;
        let ext = Path::new(&file.path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let syntax = ps
            .find_syntax_by_extension(ext)
            .filter(|_| file.lines.len() <= MAX_HIGHLIGHT_LINES);
        let theme: &Theme = &THEME_SET.themes["base16-ocean.dark"];
        let hl = syntax.map(|s| HighlightLines::new(s, theme));
        FileHighlighter {
            hl,
            lines: Vec::new(),
            next: 0,
        }
    }

    /// Highlight forward until at least `upto` diff lines are ready (clamped to
    /// the file). Cheap to call every frame - it only does the not-yet-done work,
    /// so scrolling extends the highlight a chunk at a time.
    pub fn ensure(&mut self, file: &FileDiff, upto: usize) {
        let ps = &*SYNTAX_SET;
        let target = upto.min(file.lines.len());
        while self.next < target {
            let line = highlight_one(&file.lines[self.next], self.hl.as_mut(), ps);
            self.lines.push(line);
            self.next += 1;
        }
    }

    /// The highlighted lines produced so far.
    pub fn ready(&self) -> &[Line<'static>] {
        &self.lines
    }
}

/// Render one diff line as a styled ratatui line: a coloured `+`/`-`/` ` gutter
/// then the content, syntect-highlighted for context/added lines (removed lines
/// read in red, hunk/meta dimmed).
fn highlight_one(dl: &DiffLine, hl: Option<&mut HighlightLines>, ps: &SyntaxSet) -> Line<'static> {
    match dl.kind {
        LineKind::Hunk => Line::from(Span::styled(
            dl.text.clone(),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
        )),
        LineKind::Meta => Line::from(Span::styled(
            dl.text.clone(),
            Style::default().add_modifier(Modifier::DIM),
        )),
        LineKind::Removed => Line::from(vec![
            Span::styled(
                "-",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(dl.text.clone(), Style::default().fg(Color::Red)),
        ]),
        LineKind::Context | LineKind::Added => {
            let gutter = if dl.kind == LineKind::Added {
                Span::styled(
                    "+",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(" ", Style::default().add_modifier(Modifier::DIM))
            };
            let mut spans = vec![gutter];
            spans.extend(content_spans(hl, ps, &dl.text));
            Line::from(spans)
        }
    }
}

/// Highlight one content line into owned spans. Feeds the line (with a trailing
/// newline, as syntect expects) through the stateful highlighter; falls back to a
/// single plain span when there is no syntax or highlighting errors.
fn content_spans(
    hl: Option<&mut HighlightLines>,
    ps: &SyntaxSet,
    text: &str,
) -> Vec<Span<'static>> {
    let plain = || vec![Span::raw(text.to_owned())];
    let Some(hl) = hl else {
        return plain();
    };
    let with_nl = format!("{text}\n");
    match hl.highlight_line(&with_nl, ps) {
        Ok(ranges) => ranges
            .into_iter()
            .map(|(style, piece)| {
                let fg = style.foreground;
                Span::styled(
                    piece.trim_end_matches('\n').to_owned(),
                    Style::default().fg(Color::Rgb(fg.r, fg.g, fg.b)),
                )
            })
            .filter(|s| !s.content.is_empty())
            .collect(),
        Err(_) => plain(),
    }
}

/// Case-insensitive subsequence match: every char of `query` appears in `text`
/// in order. An empty query matches everything - the file list's fuzzy filter.
pub fn fuzzy_match(query: &str, text: &str) -> bool {
    let mut hay = text.chars().flat_map(char::to_lowercase);
    'needle: for nc in query.chars().flat_map(char::to_lowercase) {
        for hc in hay.by_ref() {
            if hc == nc {
                continue 'needle;
            }
        }
        return false;
    }
    true
}

/// Run a read-only jj command, stdout on success or `None`. `--ignore-working-copy`
/// keeps it a pure read (never snapshot - that would churn commits, ADR 0006).
fn jj_read(repo_root: &Path, args: &[&str]) -> Option<String> {
    cmd("jj")
        .arg("--repository")
        .arg(repo_root)
        .arg("--ignore-working-copy")
        .args(args)
        .run()
        .ok()?
        .stdout_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Highlight a whole file - the incremental highlighter driven to completion.
    fn highlight(file: &FileDiff) -> Vec<Line<'static>> {
        let mut h = FileHighlighter::new(file);
        h.ensure(file, file.lines.len());
        h.ready().to_vec()
    }

    const SAMPLE: &str = "\
diff --git a/src/app.rs b/src/app.rs
index 1111111..2222222 100644
--- a/src/app.rs
+++ b/src/app.rs
@@ -1,4 +1,4 @@
 fn main() {
-    let x = 1;
+    let x = 2;
+    let y = 3;
 }
diff --git a/README.md b/README.md
index 3333333..4444444 100644
--- a/README.md
+++ b/README.md
@@ -1 +1 @@
-old
+new
";

    #[test]
    fn parses_files_with_per_file_counts() {
        let files = parse(SAMPLE);
        assert_eq!(files.len(), 2);

        let app = &files[0];
        assert_eq!(app.path, "src/app.rs");
        assert_eq!(app.added, 2);
        assert_eq!(app.removed, 1);
        // Header noise (index / ---/+++) is dropped; hunk + content survive.
        assert_eq!(app.lines[0].kind, LineKind::Hunk);
        assert!(
            app.lines
                .iter()
                .any(|l| l.kind == LineKind::Added && l.text == "    let y = 3;")
        );

        let readme = &files[1];
        assert_eq!(readme.path, "README.md");
        assert_eq!(readme.added, 1);
        assert_eq!(readme.removed, 1);
    }

    #[test]
    fn strips_the_one_char_prefix_but_keeps_indentation() {
        let files = parse(SAMPLE);
        let removed = files[0]
            .lines
            .iter()
            .find(|l| l.kind == LineKind::Removed)
            .unwrap();
        assert_eq!(removed.text, "    let x = 1;");
        let ctx = files[0]
            .lines
            .iter()
            .find(|l| l.kind == LineKind::Context)
            .unwrap();
        assert_eq!(ctx.text, "fn main() {");
    }

    #[test]
    fn empty_diff_yields_no_files() {
        assert!(parse("").is_empty());
        assert!(parse("   \n\n").is_empty());
    }

    #[test]
    fn highlight_degrades_to_plain_for_unknown_extension() {
        let file = FileDiff {
            path: "notes.zzz".to_string(),
            added: 1,
            removed: 0,
            lines: vec![DiffLine {
                kind: LineKind::Added,
                text: "hello world".to_string(),
            }],
        };
        let lines = highlight(&file);
        assert_eq!(lines.len(), 1);
        // Gutter span + one plain content span, no panic on an unknown language.
        let rendered: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(rendered, "+hello world");
    }

    #[test]
    fn highlights_known_language_into_multiple_spans() {
        let file = FileDiff {
            path: "x.rs".to_string(),
            added: 1,
            removed: 0,
            lines: vec![DiffLine {
                kind: LineKind::Added,
                text: "let x = 1;".to_string(),
            }],
        };
        let lines = highlight(&file);
        // syntect splits Rust into several coloured tokens beyond the gutter.
        assert!(lines[0].spans.len() > 2);
    }

    #[test]
    fn highlighter_advances_lazily_and_incrementally() {
        let file = &parse(SAMPLE)[0]; // src/app.rs, 5 diff lines
        let mut h = FileHighlighter::new(file);
        assert_eq!(h.ready().len(), 0, "nothing highlighted up front");

        h.ensure(file, 2);
        assert_eq!(h.ready().len(), 2, "only the requested prefix is done");

        h.ensure(file, 2); // idempotent - no rework
        assert_eq!(h.ready().len(), 2);

        h.ensure(file, 999); // clamped to the file length
        assert_eq!(h.ready().len(), file.lines.len());

        // Incremental output matches a one-shot highlight of the whole file.
        assert_eq!(h.ready(), highlight(file).as_slice());
    }

    #[test]
    fn fuzzy_match_is_subsequence_and_case_insensitive() {
        assert!(fuzzy_match("", "anything"));
        assert!(fuzzy_match("app", "src/app.rs"));
        assert!(fuzzy_match("sar", "src/app.rs"));
        assert!(fuzzy_match("APP", "src/app.rs"));
        assert!(!fuzzy_match("xyz", "src/app.rs"));
        assert!(!fuzzy_match("ppa", "src/app.rs"));
    }
}
