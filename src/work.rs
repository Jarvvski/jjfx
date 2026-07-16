//! The work lifecycle axis (ADR 0003): where a workspace's change sits on its
//! road to merge - Clean -> Dirty -> Pushed -> PrOpen -> Merged (CONTEXT).
//!
//! jj state is read through the CLI `-T` templates/revsets sanctioned for this
//! ticket by issue 02 (the revset engine resolves `trunk()` and diffs far more
//! simply than raw jj-lib would); PR state comes from `gh --json` (ADR 0007).
//! Trunk is whatever `trunk()` resolves to, never assumed to be `main`; PR
//! association is derived by matching a PR's head branch to a bookmark on the
//! workspace's own change chain, never hard-coded.

use std::collections::{HashMap, HashSet};
use std::path::Path;

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

/// Compute the [`Work`] snapshot for each named workspace. One `gh` call serves
/// all workspaces; jj is queried per workspace. Runs blocking subprocesses, so
/// call it from `spawn_blocking`.
///
/// Every workspace's chain is read up front so each commit can be attributed to
/// at most one workspace before classifying: a commit several workspaces share
/// (a common base they are all stacked on) must not make each of them look
/// dirty/pushed or claim the same PR - only the workspace that uniquely *heads*
/// that commit owns it, and a base nobody uniquely heads is owned by none.
pub fn snapshot(repo_root: &Path, workspaces: &[String]) -> HashMap<String, Work> {
    let prs = crate::jj::derive_repo_slug(repo_root)
        .ok()
        .flatten()
        .map(|slug| crate::prs::list(&slug))
        .unwrap_or_default();

    let chains: Vec<(String, Option<Chain>)> = workspaces
        .iter()
        .map(|name| (name.clone(), read_chain(repo_root, name)))
        .collect();
    let ownership = Ownership::compute(&chains);

    chains
        .iter()
        .map(|(name, chain)| {
            let state = match chain {
                None => WorkState::Unknown,
                Some(chain) => {
                    let owned: Vec<&ChainCommit> = chain
                        .commits
                        .iter()
                        .filter(|c| ownership.owns(name, c))
                        .collect();
                    classify(repo_root, name, &owned, &prs)
                }
            };
            let work = Work {
                state,
                behind: behind(repo_root, name),
            };
            (name.clone(), work)
        })
        .collect()
}

/// Which workspace, if any, owns each commit across the whole workspace set.
struct Ownership<'a> {
    /// change id -> how many workspaces' chains contain it.
    in_chains: HashMap<&'a str, u32>,
    /// change id -> the workspaces that head it (it is on their head-line).
    heads: HashMap<&'a str, Vec<&'a str>>,
}

impl<'a> Ownership<'a> {
    fn compute(chains: &'a [(String, Option<Chain>)]) -> Self {
        let mut in_chains: HashMap<&str, u32> = HashMap::new();
        let mut heads: HashMap<&str, Vec<&str>> = HashMap::new();
        for (name, chain) in chains {
            let Some(chain) = chain else { continue };
            for c in &chain.commits {
                *in_chains.entry(c.change_id.as_str()).or_default() += 1;
                if c.head_line {
                    heads
                        .entry(c.change_id.as_str())
                        .or_default()
                        .push(name.as_str());
                }
            }
        }
        Ownership { in_chains, heads }
    }

    /// A commit on only one chain is owned by that workspace. A commit on several
    /// is owned only by the single workspace that heads it - so a base multiple
    /// workspaces are parked on (headed by more than one) belongs to none.
    fn owns(&self, ws: &str, c: &ChainCommit) -> bool {
        let shared = self
            .in_chains
            .get(c.change_id.as_str())
            .copied()
            .unwrap_or(0)
            >= 2;
        if !shared {
            return true;
        }
        matches!(
            self.heads.get(c.change_id.as_str()),
            Some(headers) if headers.len() == 1 && headers[0] == ws
        )
    }
}

/// How far trunk has advanced past a workspace's base: the count of commits that
/// are ancestors of the trunk base but not of the workspace head
/// (`::trunk ~ ::<ws>@`). Uses the same [`crate::trunk::as_revset`] as
/// [`classify`] - the latest of the remote mainline and the local
/// `main`/`master`/`trunk` bookmarks - rather than jj's raw `trunk()`. Otherwise
/// `behind` measures against a possibly stale `origin/main` while `classify`
/// measures against local `main`, so a workspace can read `clean` yet be several
/// commits behind by the same base the dirty/clean check uses. Zero when the
/// workspace sits on the trunk tip, and zero on any jj read failure (degrade,
/// don't crash).
fn behind(repo_root: &Path, ws: &str) -> u32 {
    let trunk = crate::trunk::as_revset();
    jj(
        repo_root,
        &[
            "log",
            "-r",
            &format!("::({trunk}) ~ ::{ws}@"),
            "--no-graph",
            "-T",
            "\"x\\n\"",
        ],
    )
    .map(|out| out.lines().filter(|l| !l.is_empty()).count() as u32)
    .unwrap_or(0)
}

/// Classify one workspace from the commits it owns: overlay a matching PR, else
/// derive the state from jj facts. `owned` is the workspace's own commits (a
/// shared base is already filtered out by [`Ownership`]).
fn classify(
    repo_root: &Path,
    ws: &str,
    owned: &[&ChainCommit],
    prs: &[crate::prs::Pr],
) -> WorkState {
    match overlay(owned, prs) {
        // Fill in the line delta, measured from the base of the owned line (its
        // deepest owned commit's parent) so a shared ancestor's diff is excluded.
        WorkState::Dirty { .. } => {
            let from = owned
                .last()
                .map(|c| format!("{}-", c.change_id))
                .unwrap_or_else(crate::trunk::as_revset);
            let (added, removed) = diff_loc(repo_root, &from, ws).unwrap_or((0, 0));
            WorkState::Dirty { added, removed }
        }
        other => other,
    }
}

/// The pure overlay decision over a workspace's owned commits: PR (the furthest
/// point on the road to merge) wins, then pushed, then own content, else clean.
/// A `Dirty` result carries no line counts yet - [`classify`] fills them.
fn overlay(owned: &[&ChainCommit], prs: &[crate::prs::Pr]) -> WorkState {
    if let Some(pr) = prs.iter().find(|pr| {
        owned
            .iter()
            .any(|c| c.local_bookmarks.iter().any(|b| b == &pr.head))
    }) {
        if pr.is_merged() {
            return WorkState::Merged;
        }
        if pr.state == "OPEN" {
            return WorkState::PrOpen {
                number: pr.number,
                verdict: ReviewVerdict::parse(pr.review.as_deref()),
            };
        }
        // CLOSED-not-merged: fall back to the jj-derived state.
    }

    if owned.iter().any(|c| c.pushed) {
        return WorkState::Pushed;
    }
    if owned.iter().any(|c| !c.empty) {
        return WorkState::Dirty {
            added: 0,
            removed: 0,
        };
    }
    WorkState::Clean
}

/// One commit on a workspace's own change chain (`trunk..<ws>@`).
struct ChainCommit {
    /// The commit's change id (full, for cross-workspace ownership comparison).
    change_id: String,
    /// The commit is empty (no content of its own beyond its parent).
    empty: bool,
    /// Local bookmark names on this commit, for deriving PR association.
    local_bookmarks: Vec<String>,
    /// This commit carries a real-remote bookmark (excludes the colocated `git`
    /// pseudo-remote, via the `remote_bookmarks()` revset).
    pushed: bool,
    /// No non-empty commit sits strictly above this one in the workspace's chain,
    /// so this commit is on the workspace's head-line. Used to pick the single
    /// owner of a commit shared by several workspaces.
    head_line: bool,
}

/// A workspace's own change chain (`trunk..<ws>@`), tip first.
struct Chain {
    commits: Vec<ChainCommit>,
}

/// Read `trunk::as_revset()..<ws>@` per-commit (empty flag, change id, local
/// bookmarks) in one call, plus a second call for which commits carry a
/// real-remote bookmark. `None` on any jj failure.
fn read_chain(repo_root: &Path, ws: &str) -> Option<Chain> {
    let trunk = crate::trunk::as_revset();
    let chain = format!("({trunk})..{ws}@");

    // Per-commit, newest first: "E"/"N", change id, comma-joined local bookmarks.
    let out = jj(
        repo_root,
        &[
            "log",
            "-r",
            &chain,
            "--no-graph",
            "-T",
            "if(empty,\"E\",\"N\") ++ \"\\t\" ++ change_id ++ \"\\t\" ++ local_bookmarks.map(|b| b.name()).join(\",\") ++ \"\\n\"",
        ],
    )?;

    // Change ids on the chain that carry a real-remote bookmark. The revset
    // `remote_bookmarks()` excludes the colocated `git` remote, so this is
    // "actually pushed", not merely git-tracked.
    let pushed_out = jj(
        repo_root,
        &[
            "log",
            "-r",
            &format!("({chain}) & remote_bookmarks()"),
            "--no-graph",
            "-T",
            "change_id ++ \"\\n\"",
        ],
    )?;
    let pushed_ids: HashSet<&str> = pushed_out
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    // Walk newest -> oldest: a commit is on the head-line until a non-empty
    // commit has been seen above it.
    let mut commits = Vec::new();
    let mut seen_nonempty = false;
    for line in out.lines() {
        let mut parts = line.splitn(3, '\t');
        let flag = parts.next().unwrap_or("");
        let change_id = parts.next().unwrap_or("").trim().to_string();
        let names = parts.next().unwrap_or("");
        if change_id.is_empty() {
            continue;
        }
        let empty = flag != "N";
        let head_line = !seen_nonempty;
        if !empty {
            seen_nonempty = true;
        }
        commits.push(ChainCommit {
            pushed: pushed_ids.contains(change_id.as_str()),
            local_bookmarks: names
                .split(',')
                .filter(|n| !n.is_empty())
                .map(String::from)
                .collect(),
            change_id,
            empty,
            head_line,
        });
    }

    Some(Chain { commits })
}

/// Insertions/deletions from `from` to `<ws>@`, parsed from `jj diff --stat`.
fn diff_loc(repo_root: &Path, from: &str, ws: &str) -> Option<(u32, u32)> {
    let out = jj(
        repo_root,
        &["diff", "--from", from, "--to", &format!("{ws}@"), "--stat"],
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
    crate::jj::read_at_repo(repo_root, args).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Build a `ChainCommit` for tests.
    fn cc(
        change_id: &str,
        empty: bool,
        bms: &[&str],
        pushed: bool,
        head_line: bool,
    ) -> ChainCommit {
        ChainCommit {
            change_id: change_id.to_string(),
            empty,
            local_bookmarks: bms.iter().map(|s| s.to_string()).collect(),
            pushed,
            head_line,
        }
    }

    #[test]
    fn shared_base_headed_by_several_is_owned_by_nobody() {
        // The real bug: `uyr` (PR branch) is a shared ancestor - the head-line of
        // two empty workspaces and a buried ancestor of a third. None owns it.
        let chains = vec![
            (
                "default".to_string(),
                Some(Chain {
                    commits: vec![
                        cc("syq", false, &[], false, true),
                        cc("uyr", false, &["adam/x"], true, false),
                    ],
                }),
            ),
            (
                "new-tui".to_string(),
                Some(Chain {
                    commits: vec![
                        cc("qpo", true, &[], false, true),
                        cc("uyr", false, &["adam/x"], true, true),
                    ],
                }),
            ),
            (
                "tseter".to_string(),
                Some(Chain {
                    commits: vec![
                        cc("szw", true, &[], false, true),
                        cc("uyr", false, &["adam/x"], true, true),
                    ],
                }),
            ),
        ];
        let own = Ownership::compute(&chains);
        let uyr = cc("uyr", false, &["adam/x"], true, true);
        assert!(!own.owns("default", &uyr));
        assert!(!own.owns("new-tui", &uyr));
        assert!(!own.owns("tseter", &uyr));
        // Each workspace still owns its own tip commit.
        assert!(own.owns("default", &cc("syq", false, &[], false, true)));
        assert!(own.owns("new-tui", &cc("qpo", true, &[], false, true)));
    }

    #[test]
    fn shared_base_headed_by_one_is_still_owned_stacked_prs() {
        // A base branch `B` shared with a workspace stacked above it (`feature`)
        // is the head-line of `base` alone, so `base` keeps owning it.
        let chains = vec![
            (
                "base".to_string(),
                Some(Chain {
                    commits: vec![
                        cc("wc", true, &[], false, true),
                        cc("B", false, &["base-br"], true, true),
                    ],
                }),
            ),
            (
                "feature".to_string(),
                Some(Chain {
                    commits: vec![
                        cc("F", false, &["feat-br"], true, true),
                        cc("B", false, &["base-br"], true, false),
                    ],
                }),
            ),
        ];
        let own = Ownership::compute(&chains);
        let b = cc("B", false, &["base-br"], true, true);
        assert!(own.owns("base", &b));
        assert!(!own.owns("feature", &b));
        assert!(own.owns("feature", &cc("F", false, &["feat-br"], true, true)));
    }

    /// Build a `prs::Pr` for overlay tests.
    fn pr(number: u64, head: &str, state: &str, merged_at: Option<&str>) -> crate::prs::Pr {
        crate::prs::Pr {
            number,
            head: head.to_string(),
            state: state.to_string(),
            review: None,
            body: None,
            merged_at: merged_at.map(String::from),
        }
    }

    #[test]
    fn overlay_reads_a_merged_at_pr_as_merged() {
        // gh sometimes reports a merged PR with a non-MERGED state but a set
        // mergedAt (e.g. a squash-merge it records as CLOSED). The unified
        // `Pr::is_merged` must still classify it Merged, not fall through to the
        // jj-derived state - the divergence this consolidation removes.
        let prs = [pr(9, "adam/x", "CLOSED", Some("2026-01-01T00:00:00Z"))];
        let owned = cc("uyr", false, &["adam/x"], true, true);
        assert_eq!(overlay(&[&owned], &prs), WorkState::Merged);
    }

    #[test]
    fn overlay_prefers_pr_then_pushed_then_dirty_then_clean() {
        let prs = [pr(5, "adam/x", "OPEN", None)];
        // PR whose head branch is on an owned commit wins.
        let with_pr = cc("uyr", false, &["adam/x"], true, true);
        assert!(matches!(
            overlay(&[&with_pr], &prs),
            WorkState::PrOpen { number: 5, .. }
        ));
        // Pushed, but no matching PR.
        let pushed = cc("p", false, &["other"], true, true);
        assert_eq!(overlay(&[&pushed], &prs), WorkState::Pushed);
        // Own non-empty content, unbookmarked.
        let dirty = cc("d", false, &[], false, true);
        assert!(matches!(overlay(&[&dirty], &prs), WorkState::Dirty { .. }));
        // Only empty owned commits -> clean (the fix for a parked empty workspace).
        let empty = cc("e", true, &[], false, true);
        assert_eq!(overlay(&[&empty], &prs), WorkState::Clean);
        // Nothing owned at all -> clean.
        assert_eq!(overlay(&[], &prs), WorkState::Clean);
    }
}
