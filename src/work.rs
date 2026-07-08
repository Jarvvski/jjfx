//! The work lifecycle axis (ADR 0003): where a workspace's change sits on its
//! road to merge - Clean -> Dirty -> Pushed -> PrOpen -> Merged (CONTEXT).
//!
//! jj state is read through the CLI `-T` templates/revsets sanctioned for this
//! ticket by issue 02 (the revset engine resolves `trunk()` and diffs far more
//! simply than raw jj-lib would); PR state comes from `gh --json` (ADR 0007).
//! Trunk is whatever `trunk()` resolves to, never assumed to be `main`; PR
//! association is derived by matching a PR's head branch to a bookmark on the
//! workspace's own change chain, never hard-coded.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

/// A PR's review verdict, as reported by `gh`'s `reviewDecision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewVerdict {
    Approved,
    ChangesRequested,
    ReviewRequired,
    /// No decision yet (null/empty `reviewDecision`).
    None,
}

impl ReviewVerdict {
    fn parse(s: Option<&str>) -> Self {
        match s {
            Some("APPROVED") => ReviewVerdict::Approved,
            Some("CHANGES_REQUESTED") => ReviewVerdict::ChangesRequested,
            Some("REVIEW_REQUIRED") => ReviewVerdict::ReviewRequired,
            _ => ReviewVerdict::None,
        }
    }

    /// Short label for a list row.
    pub fn label(self) -> &'static str {
        match self {
            ReviewVerdict::Approved => "approved",
            ReviewVerdict::ChangesRequested => "changes-req",
            ReviewVerdict::ReviewRequired => "review",
            ReviewVerdict::None => "",
        }
    }

    /// Whether this verdict is a change request - the signal that turns a Waiting
    /// agent into a needs-you (the Attention derivation, ticket 06).
    pub fn is_changes_requested(self) -> bool {
        matches!(self, ReviewVerdict::ChangesRequested)
    }
}

/// Where a workspace's change sits on the road to merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkState {
    /// jj or gh could not determine the state (degrade, don't crash).
    #[default]
    Unknown,
    /// No change from trunk.
    Clean,
    /// Uncommitted or committed change, with the line delta from trunk.
    Dirty { added: u32, removed: u32 },
    /// A bookmark on the chain is on a real remote, but no PR is open.
    Pushed,
    /// A PR is open for the chain, carrying its review verdict.
    PrOpen { number: u64, verdict: ReviewVerdict },
    /// The PR merged.
    Merged,
}

/// A workspace's work-lifecycle snapshot: its [`WorkState`] plus how far
/// `trunk()` has advanced past its base (`behind`). `behind` is orthogonal to the
/// state - a Dirty or Pushed workspace can still be behind trunk - and `tidyws`
/// is its remedy (ticket 09).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Work {
    pub state: WorkState,
    /// Commits on `trunk()` the workspace's base has not yet caught up to.
    pub behind: u32,
}

impl WorkState {
    /// Short label for a list row.
    pub fn label(self) -> String {
        match self {
            WorkState::Unknown => "?".to_string(),
            WorkState::Clean => "clean".to_string(),
            WorkState::Dirty { added, removed } => format!("dirty +{added}/-{removed}"),
            WorkState::Pushed => "pushed".to_string(),
            WorkState::PrOpen { number, verdict } => {
                let v = verdict.label();
                if v.is_empty() {
                    format!("pr#{number}")
                } else {
                    format!("pr#{number} {v}")
                }
            }
            WorkState::Merged => "merged".to_string(),
        }
    }
}

/// One PR as reported by `gh pr list --json`.
#[derive(Debug, Clone, Deserialize)]
struct Pr {
    number: u64,
    #[serde(rename = "headRefName")]
    head: String,
    state: String,
    #[serde(rename = "reviewDecision")]
    review: Option<String>,
}

/// Compute the [`Work`] snapshot for each named workspace. One `gh` call serves
/// all workspaces; jj is queried per workspace. Runs blocking subprocesses, so
/// call it from `spawn_blocking`.
pub fn snapshot(repo_root: &Path, workspaces: &[String]) -> HashMap<String, Work> {
    let prs = derive_repo_slug(repo_root)
        .map(|slug| list_prs(&slug))
        .unwrap_or_default();
    workspaces
        .iter()
        .map(|name| {
            let work = Work {
                state: classify(repo_root, name, &prs),
                behind: behind(repo_root, name),
            };
            (name.clone(), work)
        })
        .collect()
}

/// How far `trunk()` has advanced past a workspace's base: the count of commits
/// that are ancestors of `trunk()` but not of the workspace head
/// (`::trunk() ~ ::<ws>@`). Zero when the workspace already sits on the tip of
/// trunk, and zero on any jj read failure (degrade, don't crash).
fn behind(repo_root: &Path, ws: &str) -> u32 {
    jj(
        repo_root,
        &[
            "log",
            "-r",
            &format!("::trunk() ~ ::{ws}@"),
            "--no-graph",
            "-T",
            "\"x\\n\"",
        ],
    )
    .map(|out| out.lines().filter(|l| !l.is_empty()).count() as u32)
    .unwrap_or(0)
}

/// Classify one workspace: read its jj change chain relative to `trunk()`, then
/// overlay any matching PR. A jj read failure yields `Unknown`.
fn classify(repo_root: &Path, ws: &str, prs: &[Pr]) -> WorkState {
    let Some(chain) = read_chain(repo_root, ws) else {
        return WorkState::Unknown;
    };

    // A PR whose head branch is a bookmark on this workspace's chain wins - it is
    // the furthest point on the road to merge.
    if let Some(pr) = prs.iter().find(|pr| chain.bookmarks.contains(&pr.head)) {
        match pr.state.as_str() {
            "MERGED" => return WorkState::Merged,
            "OPEN" => {
                return WorkState::PrOpen {
                    number: pr.number,
                    verdict: ReviewVerdict::parse(pr.review.as_deref()),
                };
            }
            _ => {} // CLOSED-not-merged: fall back to the jj-derived state
        }
    }

    if chain.pushed {
        return WorkState::Pushed;
    }
    if chain.has_content {
        let (added, removed) = diff_loc(repo_root, ws).unwrap_or((0, 0));
        return WorkState::Dirty { added, removed };
    }
    WorkState::Clean
}

/// The jj-derived facts about a workspace's own change chain (`trunk()..<ws>@`).
struct Chain {
    /// Any non-empty commit on the chain (real content beyond trunk).
    has_content: bool,
    /// A bookmark on the chain is on a real remote (excludes the colocated `git`
    /// pseudo-remote, via the `remote_bookmarks()` revset).
    pushed: bool,
    /// Local bookmark names on the chain, for deriving PR association.
    bookmarks: Vec<String>,
}

/// The mainline base a workspace's own work is measured against.
///
/// jj's `trunk()` resolves to the *remote* mainline, but a repo that has never
/// been pushed has no remote bookmark, so `trunk()` falls back to the root
/// commit - which would make every workspace diff the whole history and an empty
/// workspace read as dirty. Prefer `trunk()` when it is a real commit, else fall
/// back to the local `main`/`master`/`trunk` bookmark (whichever exists), taking
/// the most recent. `present(...)` stops a missing bookmark from erroring the
/// revset. Once main is pushed, `trunk()` wins and behaviour is unchanged.
const TRUNK_BASE: &str =
    "latest((trunk() ~ root()) | present(main) | present(master) | present(trunk))";

/// Read `<base>..<ws>@` for content flags + local bookmark names in one call,
/// plus a second call for real-remote presence. `None` on any jj failure.
fn read_chain(repo_root: &Path, ws: &str) -> Option<Chain> {
    let chain = format!("({TRUNK_BASE})..{ws}@");

    // Per-commit: "E"/"N" for empty/non-empty, then comma-joined local bookmarks.
    let out = jj(
        repo_root,
        &[
            "log",
            "-r",
            &chain,
            "--no-graph",
            "-T",
            "if(empty,\"E\",\"N\") ++ \"\\t\" ++ local_bookmarks.map(|b| b.name()).join(\",\") ++ \"\\n\"",
        ],
    )?;

    let mut has_content = false;
    let mut bookmarks = Vec::new();
    for line in out.lines() {
        let (flag, names) = line.split_once('\t').unwrap_or((line, ""));
        if flag == "N" {
            has_content = true;
        }
        for name in names.split(',').filter(|n| !n.is_empty()) {
            if !bookmarks.iter().any(|b| b == name) {
                bookmarks.push(name.to_string());
            }
        }
    }

    // Pushed iff any commit on the chain carries a real-remote bookmark. The
    // revset `remote_bookmarks()` excludes the colocated `git` remote, so this is
    // "actually pushed", not merely git-tracked.
    let pushed_out = jj(
        repo_root,
        &[
            "log",
            "-r",
            &format!("({chain}) & remote_bookmarks()"),
            "--no-graph",
            "-T",
            "\"x\"",
        ],
    )?;
    let pushed = !pushed_out.trim().is_empty();

    Some(Chain {
        has_content,
        pushed,
        bookmarks,
    })
}

/// Insertions/deletions from the mainline base to `<ws>@`, parsed from
/// `jj diff --stat`.
fn diff_loc(repo_root: &Path, ws: &str) -> Option<(u32, u32)> {
    let out = jj(
        repo_root,
        &[
            "diff",
            "--from",
            TRUNK_BASE,
            "--to",
            &format!("{ws}@"),
            "--stat",
        ],
    )?;
    parse_diff_stat(&out)
}

/// Parse the summary line of `--stat`, e.g.
/// "3 files changed, 12 insertions(+), 4 deletions(-)". Either clause may be
/// absent when there are none of that kind.
fn parse_diff_stat(stat: &str) -> Option<(u32, u32)> {
    let summary = stat.lines().last()?;
    let mut added = 0;
    let mut removed = 0;
    for clause in summary.split(',') {
        let clause = clause.trim();
        if let Some(n) = clause
            .strip_suffix(" insertions(+)")
            .or(clause.strip_suffix(" insertion(+)"))
        {
            added = n.trim().parse().unwrap_or(0);
        } else if let Some(n) = clause
            .strip_suffix(" deletions(-)")
            .or(clause.strip_suffix(" deletion(-)"))
        {
            removed = n.trim().parse().unwrap_or(0);
        }
    }
    Some((added, removed))
}

/// Run a read-only jj command, returning stdout on success or `None` on any
/// failure. `--ignore-working-copy` keeps it a pure read - jjfx must never
/// snapshot the working copy (that would churn commits and ping its own watcher).
fn jj(repo_root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("jj")
        .arg("--repository")
        .arg(repo_root)
        .arg("--ignore-working-copy")
        .args(args)
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        None
    }
}

/// List PRs via `gh --json`. Returns an empty list on any failure, so a missing
/// `gh`, no auth, or no network degrades to "no PR info" rather than crashing.
fn list_prs(slug: &str) -> Vec<Pr> {
    let output = Command::new("gh")
        .args([
            "pr",
            "list",
            "-R",
            slug,
            "--state",
            "all",
            "--limit",
            "100",
            "--json",
            "number,headRefName,state,reviewDecision",
        ])
        .output();
    match output {
        Ok(out) if out.status.success() => serde_json::from_slice(&out.stdout).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Derive the `owner/repo` slug from jj's `origin` remote URL. `gh` auto-detection
/// fails in jj workspaces, so every `gh` call must pass `-R <slug>` (CLAUDE.md).
fn derive_repo_slug(repo_root: &Path) -> Option<String> {
    let out = jj(repo_root, &["git", "remote", "list"])?;
    let url = out
        .lines()
        .filter_map(|l| l.split_once(char::is_whitespace))
        .find(|(name, _)| *name == "origin")
        .map(|(_, url)| url.trim())?;
    slug_from_url(url)
}

/// Extract `owner/repo` from an SSH (`git@host:owner/repo.git`) or HTTPS
/// (`https://host/owner/repo.git`) remote URL.
fn slug_from_url(url: &str) -> Option<String> {
    let url = url.strip_suffix(".git").unwrap_or(url);
    let parts: Vec<&str> = url.split(['/', ':']).filter(|s| !s.is_empty()).collect();
    if parts.len() >= 2 {
        Some(format!(
            "{}/{}",
            parts[parts.len() - 2],
            parts[parts.len() - 1]
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_from_ssh_and_https() {
        assert_eq!(
            slug_from_url("git@github.com:Jarvvski/jjfx.git").as_deref(),
            Some("Jarvvski/jjfx")
        );
        assert_eq!(
            slug_from_url("https://github.com/Jarvvski/jjfx.git").as_deref(),
            Some("Jarvvski/jjfx")
        );
        assert_eq!(
            slug_from_url("git@github.com:Jarvvski/jjfx").as_deref(),
            Some("Jarvvski/jjfx")
        );
        assert_eq!(slug_from_url("nonsense").as_deref(), None);
    }

    #[test]
    fn parse_stat_handles_both_clauses_and_singulars() {
        assert_eq!(
            parse_diff_stat("f | 1\n3 files changed, 12 insertions(+), 4 deletions(-)"),
            Some((12, 4))
        );
        assert_eq!(
            parse_diff_stat("1 file changed, 1 insertion(+)"),
            Some((1, 0))
        );
        assert_eq!(
            parse_diff_stat("1 file changed, 2 deletions(-)"),
            Some((0, 2))
        );
        assert_eq!(
            parse_diff_stat("0 files changed, 0 insertions(+), 0 deletions(-)"),
            Some((0, 0))
        );
    }

    #[test]
    fn review_verdict_parses_gh_values() {
        assert_eq!(
            ReviewVerdict::parse(Some("APPROVED")),
            ReviewVerdict::Approved
        );
        assert_eq!(
            ReviewVerdict::parse(Some("CHANGES_REQUESTED")),
            ReviewVerdict::ChangesRequested
        );
        assert_eq!(ReviewVerdict::parse(None), ReviewVerdict::None);
    }

    #[test]
    fn work_state_labels_read_at_a_glance() {
        assert_eq!(WorkState::Clean.label(), "clean");
        assert_eq!(
            WorkState::Dirty {
                added: 12,
                removed: 4
            }
            .label(),
            "dirty +12/-4"
        );
        assert_eq!(WorkState::Pushed.label(), "pushed");
        assert_eq!(
            WorkState::PrOpen {
                number: 7,
                verdict: ReviewVerdict::Approved
            }
            .label(),
            "pr#7 approved"
        );
        assert_eq!(
            WorkState::PrOpen {
                number: 7,
                verdict: ReviewVerdict::None
            }
            .label(),
            "pr#7"
        );
        assert_eq!(WorkState::Merged.label(), "merged");
        assert_eq!(WorkState::Unknown.label(), "?");
    }

    #[test]
    fn pr_association_matches_by_head_branch() {
        // A PR is matched only when its head branch is a bookmark on the chain.
        let prs = [Pr {
            number: 5,
            head: "feature-x".to_string(),
            state: "OPEN".to_string(),
            review: Some("CHANGES_REQUESTED".to_string()),
        }];
        // Simulate the overlay decision directly (classify's jj part needs a repo).
        let chain = Chain {
            has_content: true,
            pushed: true,
            bookmarks: vec!["feature-x".to_string()],
        };
        let matched = prs.iter().find(|pr| chain.bookmarks.contains(&pr.head));
        assert!(matched.is_some());
        // A chain without that bookmark does not match.
        let other = Chain {
            has_content: true,
            pushed: false,
            bookmarks: vec!["something-else".to_string()],
        };
        assert!(
            prs.iter()
                .find(|pr| other.bookmarks.contains(&pr.head))
                .is_none()
        );
    }
}
