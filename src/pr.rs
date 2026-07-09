//! Native stacked-PR submission over `gh` (ADR 0007), replacing the external
//! `jj-spr` shell-out so the forge depends only on `jj` and `gh`.
//!
//! Given a workspace's own bookmark chain (bottom-most first), [`submit`]:
//! 1. finds each bookmark's existing PR (an open one, else a merged one),
//! 2. creates a PR for any bookmark that lacks one - base = the nearest open
//!    bookmark below it, or the repo's default branch at the bottom of the stack,
//! 3. rewrites every open PR's body with a `## Stack` navigation section.
//!
//! Titles/bodies come from the jj commit description (first line = title). Only
//! `jj` and `gh` are invoked; nothing is assumed on `PATH` beyond those two.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::cmd::cmd;
use crate::trunk;

/// The repo-wide facts a submission pass needs, resolved once per forge run.
#[derive(Debug, Clone)]
pub struct Context {
    /// `owner/repo`, passed to every `gh` call (`-R`).
    pub slug: String,
    /// The default branch name, the base of the bottom PR in a stack.
    pub default_branch: String,
    /// Open newly-created PRs as drafts.
    pub draft: bool,
}

/// The result of a PR-submission pass for one workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// PRs were created and/or updated - real work.
    Did,
    /// Nothing to submit (no bookmark on the chain, or all PRs already current);
    /// carries a short footer reason.
    Noop(String),
    /// A `jj`/`gh` call failed; carries a short footer reason.
    Failed(String),
}

/// One PR as reported by `gh pr list --json`.
#[derive(Debug, Clone, Deserialize)]
struct GhPr {
    number: u64,
    /// GraphQL state: `OPEN` | `CLOSED` | `MERGED`.
    state: String,
    body: Option<String>,
    #[serde(rename = "mergedAt")]
    merged_at: Option<String>,
}

impl GhPr {
    fn is_merged(&self) -> bool {
        self.state == "MERGED" || self.merged_at.is_some()
    }
}

/// One bookmark on a workspace's chain, with its commit-derived title/body and
/// its resolved PR (looked up, then filled in on creation).
struct Entry {
    bookmark: String,
    title: String,
    body: String,
    pr: Option<GhPr>,
}

impl Entry {
    fn is_merged(&self) -> bool {
        self.pr.as_ref().is_some_and(GhPr::is_merged)
    }
}

/// Resolve the [`Context`] for a forge run: the repo slug (from jj's origin) and
/// the default branch (from `gh`). `None` if there is no origin remote or `gh`
/// cannot report the default branch - the PR step then reports it can't run.
pub async fn context(repo_root: PathBuf, draft: bool) -> Option<Context> {
    tokio::task::spawn_blocking(move || {
        let slug = crate::work::derive_repo_slug(&repo_root)?;
        let default_branch = default_branch(&slug).ok()?;
        Some(Context {
            slug,
            default_branch,
            draft,
        })
    })
    .await
    .ok()
    .flatten()
}

/// Create/update the PRs for one workspace's chain. Blocking `jj`/`gh` work runs
/// on a blocking thread; a panic there degrades to [`Outcome::Failed`].
pub async fn submit(ctx: Context, dir: PathBuf) -> Outcome {
    tokio::task::spawn_blocking(move || submit_blocking(&ctx, &dir))
        .await
        .unwrap_or_else(|_| Outcome::Failed("pr task panicked".into()))
}

fn submit_blocking(ctx: &Context, dir: &Path) -> Outcome {
    let bookmarks = match chain_bookmarks(dir) {
        Ok(b) => b,
        Err(e) => return Outcome::Failed(e),
    };
    if bookmarks.is_empty() {
        return Outcome::Noop("no bookmark to open a PR".into());
    }

    // Build the stack bottom-to-top, each entry carrying its existing PR (if any)
    // and its commit-derived title/body.
    let mut entries: Vec<Entry> = Vec::with_capacity(bookmarks.len());
    for bookmark in bookmarks {
        let pr = match find_pr(&ctx.slug, &bookmark) {
            Ok(pr) => pr,
            Err(e) => return Outcome::Failed(e),
        };
        let (title, body) = match jj_read(
            dir,
            &["log", "-r", &bookmark, "--no-graph", "-T", "description"],
        ) {
            Ok(desc) => split_title_body(&desc),
            Err(e) => return Outcome::Failed(e),
        };
        entries.push(Entry {
            bookmark,
            title,
            body,
            pr,
        });
    }

    let mut did_work = false;

    // Pass 1: open a PR for every bookmark that lacks one (skip merged branches).
    for i in 0..entries.len() {
        if entries[i].pr.is_some() {
            continue;
        }
        let base = base_for(&entries, i, &ctx.default_branch);
        let title = if entries[i].title.is_empty() {
            entries[i].bookmark.clone()
        } else {
            entries[i].title.clone()
        };
        match create_pr(
            &ctx.slug,
            &entries[i].bookmark,
            &base,
            &title,
            &entries[i].body,
            ctx.draft,
        ) {
            Ok(pr) => entries[i].pr = Some(pr),
            Err(e) => return Outcome::Failed(e),
        }
        did_work = true;
    }

    // Pass 2: rewrite each open PR's body with the stack section and re-assert its
    // base. Now that pass 1 has run, every non-merged entry has a PR number.
    for i in 0..entries.len() {
        let Some(pr) = &entries[i].pr else { continue };
        if pr.is_merged() {
            continue;
        }
        let (number, old_body) = (pr.number, pr.body.clone().unwrap_or_default());
        let section = stack_section(&entries, &entries[i].bookmark);
        let new_body = merge_body(&old_body, &section);
        let base = base_for(&entries, i, &ctx.default_branch);
        if let Err(e) = edit_pr(&ctx.slug, number, &new_body, &base) {
            return Outcome::Failed(e);
        }
        did_work = true;
    }

    if did_work {
        Outcome::Did
    } else {
        Outcome::Noop("PRs already current".into())
    }
}

/// The base branch for the PR at `i`: the nearest not-yet-merged bookmark below
/// it in the stack, else the repo default branch. Skipping merged ancestors keeps
/// a child PR from being based on a branch that merge deleted.
fn base_for(entries: &[Entry], i: usize, default_branch: &str) -> String {
    entries[..i]
        .iter()
        .rev()
        .find(|e| !e.is_merged())
        .map(|e| e.bookmark.clone())
        .unwrap_or_else(|| default_branch.to_string())
}

/// The `## Stack` navigation block: one line per PR in the stack, bottom-first,
/// the current one flagged and merged ones struck through. Mirrors the format
/// jj-pr-stack wrote, so existing PR bodies stay consistent.
fn stack_section(entries: &[Entry], current: &str) -> String {
    let mut lines = vec!["## Stack".to_string(), String::new()];
    for e in entries {
        let Some(pr) = &e.pr else { continue };
        let marker = if e.bookmark == current {
            "👉🏻 "
        } else {
            ""
        };
        let reference = if e.is_merged() {
            format!("~~#{}~~", pr.number)
        } else {
            format!("#{}", pr.number)
        };
        lines.push(format!("- {marker}{reference}"));
    }
    lines.join("\n")
}

/// Replace any existing `## Stack` section in `existing` with `section`, appended
/// after the rest of the body. A stack section runs from its heading to the next
/// top-level `## ` heading (or end of body), so other sections are preserved.
fn merge_body(existing: &str, section: &str) -> String {
    let clean = strip_stack_section(existing);
    let clean = clean.trim();
    if clean.is_empty() {
        section.to_string()
    } else {
        format!("{clean}\n\n{section}")
    }
}

/// Cut the `## Stack` block (heading through the char before the next `## `
/// heading, or end) out of `body`. No stack heading -> `body` unchanged.
fn strip_stack_section(body: &str) -> String {
    let Some(start) = find_heading(body, "## Stack") else {
        return body.to_string();
    };
    let rest = start + "## Stack".len();
    let end = body[rest..]
        .find("\n## ")
        .map(|off| rest + off)
        .unwrap_or(body.len());
    let mut out = String::with_capacity(body.len());
    out.push_str(&body[..start]);
    out.push_str(&body[end..]);
    out
}

/// Byte index of `heading` where it begins a line (start of body or just after a
/// newline), so `## Stack` inside `### Stack` or mid-line prose is not matched.
fn find_heading(body: &str, heading: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(rel) = body[from..].find(heading) {
        let idx = from + rel;
        if idx == 0 || body.as_bytes()[idx - 1] == b'\n' {
            return Some(idx);
        }
        from = idx + heading.len();
    }
    None
}

/// Split a jj description into (title, body): first line, then the remainder.
fn split_title_body(desc: &str) -> (String, String) {
    let desc = desc.trim_end();
    let mut lines = desc.lines();
    let title = lines.next().unwrap_or("").trim().to_string();
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    (title, body)
}

/// The PR number at the end of a `gh pr create` URL (`.../pull/123`).
fn pr_number_from_url(url: &str) -> Option<u64> {
    url.trim().rsplit('/').next()?.parse().ok()
}

/// The bookmarks on a workspace's own chain (`trunk..@`), bottom (nearest trunk)
/// first, deduped. Read-only (`--ignore-working-copy`), run in the workspace dir
/// so `@` is that workspace's head.
fn chain_bookmarks(dir: &Path) -> Result<Vec<String>, String> {
    let trunk = trunk::as_revset();
    let revset = format!("({trunk})..@");
    let out = jj_read(
        dir,
        &[
            "log",
            "-r",
            &revset,
            "--no-graph",
            "-T",
            "local_bookmarks.map(|b| b.name()).join(\"\\n\") ++ \"\\n\"",
        ],
    )?;
    // jj logs newest-first; reverse to bottom-to-top, then dedupe preserving order.
    let mut seen = std::collections::HashSet::new();
    let names: Vec<String> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .filter(|n| seen.insert(n.clone()))
        .collect();
    Ok(names)
}

/// Find the PR for a branch: an open one wins, else a merged one; closed-unmerged
/// and absent both yield `None`.
fn find_pr(slug: &str, branch: &str) -> Result<Option<GhPr>, String> {
    let out = cmd("gh")
        .args([
            "pr",
            "list",
            "-R",
            slug,
            "--head",
            branch,
            "--state",
            "all",
            "--json",
            "number,state,body,mergedAt",
            "--limit",
            "20",
        ])
        .run()
        .map_err(|e| e.to_string())?;
    if !out.ok() {
        return Err(format!("gh pr list ({branch}): {}", out.stderr().trim()));
    }
    let prs: Vec<GhPr> = serde_json::from_str(out.stdout()).map_err(|e| e.to_string())?;
    let open = prs.iter().find(|p| p.state == "OPEN").cloned();
    Ok(open.or_else(|| prs.into_iter().find(GhPr::is_merged)))
}

/// Open a draft (or ready) PR for `head` based on `base`, returning the created
/// PR. `gh pr create` prints the new PR's URL, from which the number is parsed.
fn create_pr(
    slug: &str,
    head: &str,
    base: &str,
    title: &str,
    body: &str,
    draft: bool,
) -> Result<GhPr, String> {
    let mut args = vec![
        "pr", "create", "-R", slug, "--head", head, "--base", base, "--title", title, "--body",
        body,
    ];
    if draft {
        args.push("--draft");
    }
    let out = cmd("gh").args(args).run().map_err(|e| e.to_string())?;
    if !out.ok() {
        return Err(format!("gh pr create ({head}): {}", out.stderr().trim()));
    }
    let number = pr_number_from_url(out.stdout())
        .ok_or_else(|| format!("gh pr create ({head}): no PR url in output"))?;
    Ok(GhPr {
        number,
        state: "OPEN".into(),
        body: Some(body.to_string()),
        merged_at: None,
    })
}

/// Update a PR's body and base.
fn edit_pr(slug: &str, number: u64, body: &str, base: &str) -> Result<(), String> {
    let n = number.to_string();
    let out = cmd("gh")
        .args([
            "pr",
            "edit",
            n.as_str(),
            "-R",
            slug,
            "--body",
            body,
            "--base",
            base,
        ])
        .run()
        .map_err(|e| e.to_string())?;
    if out.ok() {
        Ok(())
    } else {
        Err(format!("gh pr edit #{number}: {}", out.stderr().trim()))
    }
}

/// The default branch name via `gh` (the base of the bottom PR in a stack).
fn default_branch(slug: &str) -> Result<String, String> {
    let out = cmd("gh")
        .args([
            "repo",
            "view",
            slug,
            "--json",
            "defaultBranchRef",
            "--jq",
            ".defaultBranchRef.name",
        ])
        .run()
        .map_err(|e| e.to_string())?;
    if !out.ok() {
        return Err(format!("gh repo view ({slug}): {}", out.stderr().trim()));
    }
    let name = out.stdout().trim().to_string();
    if name.is_empty() {
        Err("gh reported an empty default branch".into())
    } else {
        Ok(name)
    }
}

/// Read-only jj command in the workspace dir (`--ignore-working-copy`, so jjfx
/// never snapshots and churns the working copy). Stdout on success, else an error
/// carrying the trimmed stderr.
fn jj_read(dir: &Path, args: &[&str]) -> Result<String, String> {
    crate::jj::read_in_dir(dir, args).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(bookmark: &str, number: u64, state: &str, merged: bool) -> Entry {
        Entry {
            bookmark: bookmark.to_string(),
            title: String::new(),
            body: String::new(),
            pr: Some(GhPr {
                number,
                state: state.to_string(),
                body: None,
                merged_at: merged.then(|| "2026-01-01T00:00:00Z".to_string()),
            }),
        }
    }

    #[test]
    fn split_title_body_takes_first_line_then_rest() {
        assert_eq!(
            split_title_body("Add a thing\n\nBody line one\nBody line two\n"),
            (
                "Add a thing".to_string(),
                "Body line one\nBody line two".to_string()
            )
        );
        assert_eq!(
            split_title_body("just a title"),
            ("just a title".to_string(), String::new())
        );
        assert_eq!(split_title_body("   "), (String::new(), String::new()));
    }

    #[test]
    fn pr_number_parses_from_a_create_url() {
        assert_eq!(
            pr_number_from_url("https://github.com/o/r/pull/42\n"),
            Some(42)
        );
        assert_eq!(pr_number_from_url("not a url"), None);
    }

    #[test]
    fn stack_section_flags_current_and_strikes_merged() {
        let entries = vec![
            entry("base", 10, "MERGED", true),
            entry("mid", 11, "OPEN", false),
            entry("top", 12, "OPEN", false),
        ];
        let section = stack_section(&entries, "mid");
        assert_eq!(section, "## Stack\n\n- ~~#10~~\n- 👉🏻 #11\n- #12");
    }

    #[test]
    fn stack_section_skips_bookmarks_without_a_pr() {
        let mut entries = vec![entry("a", 1, "OPEN", false)];
        entries.push(Entry {
            bookmark: "b".into(),
            title: String::new(),
            body: String::new(),
            pr: None,
        });
        assert_eq!(stack_section(&entries, "a"), "## Stack\n\n- 👉🏻 #1");
    }

    #[test]
    fn merge_body_appends_when_no_prior_section() {
        assert_eq!(
            merge_body("Original body.", "## Stack\n\n- #1"),
            "Original body.\n\n## Stack\n\n- #1"
        );
        // Empty body -> just the section.
        assert_eq!(merge_body("", "## Stack\n\n- #1"), "## Stack\n\n- #1");
    }

    #[test]
    fn merge_body_replaces_an_existing_stack_section() {
        let existing = "Intro.\n\n## Stack\n\n- #1\n- #2";
        assert_eq!(
            merge_body(existing, "## Stack\n\n- #1\n- #2\n- #3"),
            "Intro.\n\n## Stack\n\n- #1\n- #2\n- #3"
        );
    }

    #[test]
    fn merge_body_preserves_a_section_after_the_stack() {
        let existing = "Intro.\n\n## Stack\n\n- #1\n\n## Notes\n\nkeep me";
        let merged = merge_body(existing, "## Stack\n\n- #1\n- #2");
        assert!(
            merged.contains("## Notes\n\nkeep me"),
            "notes dropped: {merged}"
        );
        assert!(merged.contains("- #2"), "new stack missing: {merged}");
        // The old single-item stack must be gone, replaced by the two-item one.
        assert!(
            !merged.contains("- #1\n\n## Notes"),
            "old stack kept: {merged}"
        );
    }

    #[test]
    fn find_heading_ignores_deeper_headings_and_prose() {
        assert_eq!(find_heading("## Stack\n- #1", "## Stack"), Some(0));
        assert_eq!(find_heading("x\n## Stack\n", "## Stack"), Some(2));
        // "### Stack" contains "## Stack" but is not a line-start match.
        assert_eq!(find_heading("### Stack\n", "## Stack"), None);
        assert_eq!(find_heading("see the ## Stack here", "## Stack"), None);
    }

    #[test]
    fn base_for_skips_merged_ancestors_down_to_trunk() {
        let entries = vec![
            entry("base", 10, "MERGED", true),
            entry("mid", 11, "OPEN", false),
            entry("top", 12, "OPEN", false),
        ];
        // Bottom PR bases on trunk.
        assert_eq!(base_for(&entries, 0, "main"), "main");
        // 'mid' would base on 'base', but it's merged -> fall through to trunk.
        assert_eq!(base_for(&entries, 1, "main"), "main");
        // 'top' bases on the nearest open ancestor, 'mid'.
        assert_eq!(base_for(&entries, 2, "main"), "mid");
    }
}
