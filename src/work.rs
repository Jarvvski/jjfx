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

    fn blocking_rank(self) -> u8 {
        match self {
            ReviewVerdict::ChangesRequested => 3,
            ReviewVerdict::ReviewRequired => 2,
            ReviewVerdict::None => 1,
            ReviewVerdict::Approved => 0,
        }
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
    /// A commit on the chain carries an unresolved conflict, naming it.
    ///
    /// jj stores a conflict *in the commit* and a rebase that hits one succeeds
    /// and records it, so this is a normal, recoverable state - not a failure.
    /// It is its own Work state rather than a silent skip: a conflicted chain
    /// cannot be forged (the forge refuses to push it), so it is work waiting on
    /// the human. The name is the conflicted commit that *owns* the conflict -
    /// the deepest one, since every commit above it merely inherits it.
    Conflicted { commit: ChangeId },
}

/// The length of the change-id prefix a surfaced conflict names, matching jj's
/// own default short display (`change_id.short()`).
const SHORT_CHANGE_ID_LEN: usize = 12;

/// A short jj change-id prefix, carried inline so [`WorkState`] stays `Copy`.
///
/// The full change id is 40+ hex characters - too long for a list row - and jj
/// itself shows a prefix. A prefix is a valid revset, so the named commit is
/// something the owner can act on directly. The *change* id is named rather than
/// the commit id because the commit id changes the moment the conflict is
/// resolved, while the change id is the identity jj's own commands take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ChangeId([u8; SHORT_CHANGE_ID_LEN]);

impl ChangeId {
    /// The short form of `change_id`: its first [`SHORT_CHANGE_ID_LEN`] bytes.
    pub fn short(change_id: &str) -> Self {
        let mut bytes = [0u8; SHORT_CHANGE_ID_LEN];
        for (slot, byte) in bytes.iter_mut().zip(change_id.bytes()) {
            *slot = byte;
        }
        Self(bytes)
    }

    /// The short change id as text. Trailing padding is trimmed, so a change id
    /// shorter than the prefix is reported whole rather than with NUL bytes.
    pub fn as_str(&self) -> &str {
        let end = self
            .0
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(self.0.len());
        std::str::from_utf8(&self.0[..end]).unwrap_or_default()
    }
}

impl std::fmt::Display for ChangeId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
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
    /// Whether this state means the Workspace's work has left it.
    ///
    /// `Clean` has nothing to deliver, `Pushed` and `PrOpen` are on a real
    /// remote, and `Merged` is in trunk. `Dirty` and `Conflicted` exist only in
    /// the Workspace, and `Unknown` is a failed read - which must not read as
    /// delivered, because "could not tell" and "safe to discard" are different
    /// facts.
    pub fn is_delivered(self) -> bool {
        matches!(
            self,
            WorkState::Clean | WorkState::Pushed | WorkState::PrOpen { .. } | WorkState::Merged
        )
    }

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
            WorkState::Conflicted { commit } => format!("conflict {commit}"),
        }
    }
}

/// The read port behind the Work snapshot: the jj and gh facts classification
/// needs. Classification runs through the same interface callers use, so a test
/// adapter substitutes exactly the facts that vary in production.
pub(crate) trait WorkReads {
    /// Every PR reported for the repository, in one read.
    ///
    /// A failure must stay distinguishable from an empty list: a Workspace whose
    /// Pull Request has merged looks `Pushed` when the read fails, so reporting
    /// that as "no Pull Requests" would silently mark it as still needing a
    /// forge.
    fn pull_requests(&self) -> Result<Vec<crate::prs::Pr>, String>;
    /// One Workspace's own change chain (`trunk..<ws>@`), tip first. `None` on a
    /// read failure, rendered as [`WorkState::Unknown`].
    fn chain(&self, workspace: &str) -> Option<Chain>;
    /// How far trunk has advanced past the Workspace's base. `Err` when jj could
    /// not be read - a failed read must not report "0 behind".
    fn behind(&self, workspace: &str) -> Result<u32, String>;
    /// Insertions/deletions from the base of the owned line to the Workspace's
    /// `@`. `base` is the deepest owned commit's change id, or `None` when the
    /// line reaches the trunk revset itself. `Err` when jj could not be read -
    /// a failed diff must not be reported as a zero delta.
    fn line_delta(&self, workspace: &str, base: Option<&str>) -> Result<(u32, u32), String>;
}

/// The production adapter: jj CLI reads plus one `gh pr list` (ADR 0007).
pub(crate) struct SystemWorkReads {
    repo_root: std::path::PathBuf,
}

impl SystemWorkReads {
    /// Bind the port to one repository root.
    pub(crate) fn new(repo_root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            repo_root: repo_root.into(),
        }
    }
}

impl WorkReads for SystemWorkReads {
    fn pull_requests(&self) -> Result<Vec<crate::prs::Pr>, String> {
        match crate::jj::derive_repo_slug(&self.repo_root) {
            // Not a GitHub Repository: there is nothing to read, and that is not a
            // failure.
            Ok(None) => Ok(Vec::new()),
            Ok(Some(slug)) => crate::prs::list(&slug),
            Err(error) => Err(error.to_string()),
        }
    }

    fn chain(&self, workspace: &str) -> Option<Chain> {
        read_chain(&self.repo_root, workspace)
    }

    fn behind(&self, workspace: &str) -> Result<u32, String> {
        behind(&self.repo_root, workspace).map_err(|error| error.to_string())
    }

    fn line_delta(&self, workspace: &str, base: Option<&str>) -> Result<(u32, u32), String> {
        let from = base
            .map(|change_id| format!("{change_id}-"))
            .unwrap_or_else(crate::trunk::as_revset);
        let to = format!("{}@", crate::jj::revset_symbol(workspace));
        crate::jj::diff_line_delta(&self.repo_root, &from, &to).map_err(|error| error.to_string())
    }
}

/// Compute the [`Work`] snapshot for each named workspace through the read port.
/// One PR read serves all workspaces; chains are read up front so each commit can
/// be attributed to at most one workspace before classifying: a commit several
/// workspaces share (a common base they are all stacked on) must not make each of
/// them look dirty/pushed or claim the same PR - only the workspace that uniquely
/// *heads* that commit owns it, and a base nobody uniquely heads is owned by
/// none.
pub fn snapshot(reads: &dyn WorkReads, workspaces: &[String]) -> HashMap<String, Work> {
    // A failed Pull Request read is not "no Pull Requests": it means
    // classification cannot tell a pushed commit from one with an open or merged
    // Pull Request, so it must not claim the former.
    let (prs, prs_known) = match reads.pull_requests() {
        Ok(prs) => (prs, true),
        Err(_) => (Vec::new(), false),
    };

    let chains: Vec<(String, Option<Chain>)> = workspaces
        .iter()
        .map(|name| (name.clone(), reads.chain(name)))
        .collect();
    let ownership = Ownership::compute(&chains);

    chains
        .iter()
        .map(|(name, chain)| {
            let state = match chain {
                None => WorkState::Unknown,
                // A conflicted chain is its own state, read over the *whole*
                // chain rather than the owned subset: the forge refuses to push
                // exactly `(::@ ~ trunk()) & conflicts()`, so classifying the
                // same set keeps the surfaced state and that refusal in
                // agreement instead of letting a shared conflicted base hide.
                Some(chain) => match chain.conflict() {
                    Some(commit) => WorkState::Conflicted { commit },
                    None => {
                        let owned: Vec<&ChainCommit> = chain
                            .commits
                            .iter()
                            .filter(|c| ownership.owns(name, c))
                            .collect();
                        classify(reads, name, &owned, &prs, prs_known)
                    }
                },
            };
            let work = match reads.behind(name) {
                Ok(behind) => Work { state, behind },
                // A failed behind read must not report "0 behind": degrade the
                // row to unknown so the failure is visible, not a plausible zero.
                Err(_) => Work {
                    state: WorkState::Unknown,
                    behind: 0,
                },
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
/// workspace sits on the trunk tip; a jj failure is a typed error, never a zero.
fn behind(repo_root: &Path, ws: &str) -> anyhow::Result<u32> {
    let trunk = crate::trunk::as_revset();
    let out = crate::jj::read_at_repo(
        repo_root,
        &[
            "log",
            "-r",
            &format!("::({trunk}) ~ ::{}@", crate::jj::revset_symbol(ws)),
            "--no-graph",
            "-T",
            "\"x\\n\"",
        ],
    )?;
    Ok(out.lines().filter(|l| !l.is_empty()).count() as u32)
}

/// Classify one workspace from the commits it owns: overlay a matching PR, else
/// derive the state from jj facts. `owned` is the workspace's own commits (a
/// shared base is already filtered out by [`Ownership`]).
fn classify(
    reads: &dyn WorkReads,
    ws: &str,
    owned: &[&ChainCommit],
    prs: &[crate::prs::Pr],
    prs_known: bool,
) -> WorkState {
    match overlay(owned, prs) {
        // Fill in the line delta, measured from the base of the owned line (its
        // deepest owned commit's parent) so a shared ancestor's diff is excluded.
        WorkState::Dirty { .. } => {
            let base = owned.last().map(|c| c.change_id.as_str());
            match reads.line_delta(ws, base) {
                Ok((added, removed)) => WorkState::Dirty { added, removed },
                // A failed diff read must not claim a zero delta - report the
                // state as unknown rather than a plausible-looking clean zero.
                Err(_) => WorkState::Unknown,
            }
        }
        // Without a Pull Request read, a pushed commit may already have an open
        // or merged Pull Request. Say the state is unknown rather than claiming it
        // still needs a forge.
        WorkState::Pushed if !prs_known => WorkState::Unknown,
        other => other,
    }
}

/// The pure overlay decision over a workspace's owned commits. The least
/// delivered change wins: local content, then pushed, then an open PR, then
/// merged - with one exception. A lower open **changes-requested** PR is never
/// masked by newer local or pushed work stacked above it (ticket 31, ADR 0003):
/// the PR is blocked on the human, so it must still surface as
/// [`WorkState::PrOpen`] and reach `Attention::NeedsYou` rather than reading as
/// merely "ready to forge". A `Dirty` result carries no line counts yet -
/// [`classify`] fills it.
fn overlay(owned: &[&ChainCommit], prs: &[crate::prs::Pr]) -> WorkState {
    // Resolve the row's open PR up front: only a change request escapes the
    // least-delivered rule, so an approved (or verdict-less) lower PR still
    // yields Dirty/Pushed below.
    let selected = select_open_pr(owned, prs);
    if let Some((pr, verdict)) = selected
        && verdict.is_changes_requested()
    {
        return WorkState::PrOpen {
            number: pr.number,
            verdict,
        };
    }

    if owned
        .iter()
        .any(|commit| !commit.empty && !commit.pushed && !represented_by_pr(commit, prs))
    {
        return WorkState::Dirty {
            added: 0,
            removed: 0,
        };
    }

    if owned
        .iter()
        .any(|commit| commit.pushed && !represented_by_pr(commit, prs))
    {
        return WorkState::Pushed;
    }

    if let Some((pr, verdict)) = selected {
        return WorkState::PrOpen {
            number: pr.number,
            verdict,
        };
    }

    if owned
        .iter()
        .any(|commit| merged_pr_for(commit, prs).is_some())
    {
        return WorkState::Merged;
    }

    WorkState::Clean
}

/// The open PR the row should report, if any: the lowest PR carrying the most
/// blocking verdict across the owned commits. `owned` is tip-first, so a tied
/// verdict replaces the current pick and selects the lower PR.
fn select_open_pr<'a>(
    owned: &[&ChainCommit],
    prs: &'a [crate::prs::Pr],
) -> Option<(&'a crate::prs::Pr, ReviewVerdict)> {
    let mut selected: Option<(&'a crate::prs::Pr, ReviewVerdict)> = None;
    for commit in owned {
        let Some(pr) = open_pr_for(commit, prs) else {
            continue;
        };
        let verdict = ReviewVerdict::parse(pr.review.as_deref());
        let replace = selected
            .as_ref()
            .is_none_or(|(_, current)| verdict.blocking_rank() >= current.blocking_rank());
        if replace {
            selected = Some((pr, verdict));
        }
    }
    selected
}

fn represented_by_pr(commit: &ChainCommit, prs: &[crate::prs::Pr]) -> bool {
    open_pr_for(commit, prs).is_some() || merged_pr_for(commit, prs).is_some()
}

fn open_pr_for<'a>(commit: &ChainCommit, prs: &'a [crate::prs::Pr]) -> Option<&'a crate::prs::Pr> {
    prs.iter()
        .find(|pr| pr.state == "OPEN" && !pr.is_merged() && pr_matches_commit(pr, commit))
}

fn merged_pr_for<'a>(
    commit: &ChainCommit,
    prs: &'a [crate::prs::Pr],
) -> Option<&'a crate::prs::Pr> {
    prs.iter()
        .find(|pr| pr.is_merged() && pr_matches_commit(pr, commit))
}

fn pr_matches_commit(pr: &crate::prs::Pr, commit: &ChainCommit) -> bool {
    commit
        .local_bookmarks
        .iter()
        .any(|bookmark| bookmark == &pr.head)
}

/// One commit on a workspace's own change chain (`trunk..<ws>@`).
#[derive(Debug, Clone)]
pub(crate) struct ChainCommit {
    /// The commit's change id (full, for cross-workspace ownership comparison).
    change_id: String,
    /// The commit is empty (no content of its own beyond its parent).
    empty: bool,
    /// Local bookmark names on this commit, for deriving PR association.
    local_bookmarks: Vec<String>,
    /// This commit carries a real-remote bookmark (excludes the colocated `git`
    /// pseudo-remote, via the `remote_bookmarks()` revset).
    pushed: bool,
    /// The commit's tree holds an unresolved conflict. A commit above a
    /// conflicted one inherits the conflict, so several commits on a chain
    /// report true for one conflict.
    conflicted: bool,
    /// No non-empty commit sits strictly above this one in the workspace's chain,
    /// so this commit is on the workspace's head-line. Used to pick the single
    /// owner of a commit shared by several workspaces.
    head_line: bool,
}

/// A workspace's own change chain (`trunk..<ws>@`), tip first.
#[derive(Debug, Clone)]
pub(crate) struct Chain {
    commits: Vec<ChainCommit>,
}

impl Chain {
    /// The conflicted commit that owns this chain's conflict, if any.
    ///
    /// `commits` is tip-first, so the *deepest* conflicted commit is the owner:
    /// a conflict is recorded in the commit the rebase rewrote, and every commit
    /// above it merely inherits it. Resolving the owner clears them all, which
    /// is why the owner is the commit to name and to resolve.
    pub(crate) fn conflict(&self) -> Option<ChangeId> {
        self.commits
            .iter()
            .rev()
            .find(|commit| commit.conflicted)
            .map(|commit| ChangeId::short(&commit.change_id))
    }
}

/// Read `trunk::as_revset()..<ws>@` per-commit (empty flag, change id, local
/// bookmarks) in one call, plus a second call for which commits carry a
/// real-remote bookmark. `None` on any jj failure.
fn read_chain(repo_root: &Path, ws: &str) -> Option<Chain> {
    let trunk = crate::trunk::as_revset();
    let chain = format!("({trunk})..{}@", crate::jj::revset_symbol(ws));

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

    // Change ids on the chain whose tree holds an unresolved conflict. jj's
    // `conflicts()` revset is the machine-readable source - the same one the
    // forge's push gate uses - so the state and the refusal agree by
    // construction rather than by parsing jj's status prose.
    let conflicted_out = jj(
        repo_root,
        &[
            "log",
            "-r",
            &format!("({chain}) & conflicts()"),
            "--no-graph",
            "-T",
            "change_id ++ \"\\n\"",
        ],
    )?;
    let conflicted_ids: HashSet<&str> = conflicted_out
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
            conflicted: conflicted_ids.contains(change_id.as_str()),
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

    /// `pool destroy` refuses to discard a Worker's branch unless the work has
    /// already left the Workspace, so "delivered" must mean exactly that:
    /// nothing to deliver (clean), on a real remote (pushed or an open PR), or
    /// in trunk (merged). A failed read is not delivered - "could not tell" and
    /// "safe to discard" are different facts.
    #[test]
    fn only_delivered_work_is_safe_to_discard() {
        for delivered in [
            WorkState::Clean,
            WorkState::Pushed,
            WorkState::Merged,
            WorkState::PrOpen {
                number: 7,
                verdict: ReviewVerdict::Approved,
            },
        ] {
            assert!(delivered.is_delivered(), "{delivered:?} is delivered");
        }
        for held in [
            WorkState::Dirty {
                added: 1,
                removed: 0,
            },
            WorkState::Conflicted {
                commit: ChangeId::short("kqxsylvyrwvq"),
            },
            WorkState::Unknown,
        ] {
            assert!(!held.is_delivered(), "{held:?} is held in the Workspace");
        }
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
            conflicted: false,
            head_line,
        }
    }

    /// Build a `ChainCommit` whose tree holds an unresolved conflict.
    fn cc_conflicted(change_id: &str, empty: bool, head_line: bool) -> ChainCommit {
        ChainCommit {
            conflicted: true,
            ..cc(change_id, empty, &[], false, head_line)
        }
    }

    #[test]
    fn a_conflicted_chain_reads_conflicted_and_names_its_owner() {
        // The conflict is recorded in the rewritten commit and *inherited* by
        // every commit above it, so the chain reports several conflicted commits
        // for one conflict. The owner - the deepest - is the one named, because
        // that is where a resolution belongs and where resolving clears the rest.
        let fake = FakeWorkReads {
            chains: HashMap::from([(
                "feat".to_string(),
                chain(vec![
                    cc_conflicted("tip", true, true),
                    cc_conflicted("owner", false, true),
                    cc("base", false, &[], false, false),
                ]),
            )]),
            ..FakeWorkReads::default()
        };

        let work = snapshot(&fake, &["feat".to_string()]);

        assert_eq!(
            work["feat"].state,
            WorkState::Conflicted {
                commit: ChangeId::short("owner")
            }
        );
        assert_eq!(work["feat"].state.label(), "conflict owner");
    }

    #[test]
    fn a_chain_with_no_conflict_is_not_conflicted() {
        let fake = FakeWorkReads {
            chains: HashMap::from([(
                "feat".to_string(),
                chain(vec![cc("tip", true, &[], false, true)]),
            )]),
            ..FakeWorkReads::default()
        };

        assert_eq!(
            snapshot(&fake, &["feat".to_string()])["feat"].state,
            WorkState::Clean
        );
    }

    #[test]
    fn the_conflict_is_read_over_the_whole_chain_not_the_owned_subset() {
        // A conflict in a shared base is still the workspace's problem: the forge
        // refuses to push exactly `(::@ ~ trunk()) & conflicts()`, so the state
        // must not filter the conflict through commit ownership and report the
        // workspace as merely dirty.
        let fake = FakeWorkReads {
            chains: HashMap::from([
                (
                    "a".to_string(),
                    chain(vec![
                        cc("shared", false, &[], false, true),
                        cc_conflicted("conflict", false, false),
                    ]),
                ),
                (
                    "b".to_string(),
                    chain(vec![
                        cc("shared", false, &[], false, true),
                        cc_conflicted("conflict", false, false),
                    ]),
                ),
            ]),
            ..FakeWorkReads::default()
        };

        let work = snapshot(&fake, &["a".to_string(), "b".to_string()]);

        assert_eq!(
            work["a"].state,
            WorkState::Conflicted {
                commit: ChangeId::short("conflict")
            }
        );
        assert_eq!(
            work["b"].state,
            WorkState::Conflicted {
                commit: ChangeId::short("conflict")
            }
        );
    }

    #[test]
    fn a_change_id_is_shown_the_way_jj_shows_it() {
        assert_eq!(
            ChangeId::short("kqxsylvyrwvqwmpszlmnv").as_str(),
            "kqxsylvyrwvq"
        );
        assert_eq!(ChangeId::short("short").as_str(), "short");
        assert_eq!(ChangeId::short("kqxsylvyrwvq").to_string(), "kqxsylvyrwvq");
    }

    /// Run one jj command in `dir`, asserting it succeeds.
    fn jj(dir: &Path, args: &[&str]) {
        let output = crate::jj::command()
            .current_dir(dir)
            .args(args)
            .output()
            .expect("jj should be installed");
        assert!(
            output.status.success(),
            "jj {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A repository whose `ws-a` chain holds a real rebase conflict: `ws-a` and
    /// `ws-b` each rewrite the same line from the shared base, then `ws-a` is
    /// rebased onto `ws-b`. jj records the conflict in the commit and exits zero -
    /// a normal, recoverable state, not a failure.
    fn conflicted_repo(tag: &str) -> std::path::PathBuf {
        let root = crate::store::test_local_repo(tag);
        std::fs::write(root.join("f.txt"), "base\n").expect("write base file");
        jj(&root, &["commit", "-m", "base"]);
        jj(&root, &["bookmark", "create", "main", "-r", "@-"]);
        let ws_a = root.with_extension("ws-a");
        let ws_b = root.with_extension("ws-b");
        jj(
            &root,
            &[
                "workspace",
                "add",
                "--name",
                "ws-a",
                "--no-colocate",
                &ws_a.to_string_lossy(),
            ],
        );
        jj(
            &root,
            &[
                "workspace",
                "add",
                "--name",
                "ws-b",
                "--no-colocate",
                &ws_b.to_string_lossy(),
            ],
        );
        std::fs::write(ws_a.join("f.txt"), "a\n").expect("write a");
        jj(&ws_a, &["commit", "-m", "a change"]);
        std::fs::write(ws_b.join("f.txt"), "b\n").expect("write b");
        jj(&ws_b, &["commit", "-m", "b change"]);
        jj(&root, &["rebase", "-s", "\"ws-a\"@-", "-d", "\"ws-b\"@-"]);
        root
    }

    #[test]
    fn a_real_rebase_conflict_reads_conflicted_and_names_the_commit_jj_names() {
        let root = conflicted_repo("work-conflict");
        let reads = SystemWorkReads::new(root.clone());

        let work = snapshot(&reads, &["ws-a".to_string(), "ws-b".to_string()]);

        let WorkState::Conflicted { commit } = work["ws-a"].state else {
            panic!(
                "a conflicted chain must read conflicted, got {:?}",
                work["ws-a"]
            );
        };
        // The named commit is the one jj's own machine-readable conflict revset
        // reports as the chain's deepest conflicted commit.
        let conflicted = crate::jj::read_at_repo(
            &root,
            &[
                "log",
                "-r",
                "(::\"ws-a\"@ ~ trunk()) & conflicts()",
                "--no-graph",
                "-T",
                "change_id ++ \"\\n\"",
            ],
        )
        .expect("conflict read");
        let deepest = conflicted
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .expect("a conflicted commit");
        assert_eq!(commit.as_str(), ChangeId::short(deepest).as_str());
        assert_eq!(work["ws-a"].state.label(), format!("conflict {commit}"));

        // The sibling workspace, whose own chain is untouched, is not conflicted.
        assert!(
            !matches!(work["ws-b"].state, WorkState::Conflicted { .. }),
            "an unrelated chain must not read conflicted: {:?}",
            work["ws-b"]
        );
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

    fn reviewed_pr(number: u64, head: &str, review: &str) -> crate::prs::Pr {
        let mut pr = pr(number, head, "OPEN", None);
        pr.review = Some(review.to_string());
        pr
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
    fn overlay_classifies_each_single_stage() {
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

    #[test]
    fn overlay_prefers_local_dirty_change_over_lower_open_pr() {
        let local = cc("local", false, &[], false, true);
        let lower = cc("lower", false, &["lower-branch"], true, false);
        let prs = [reviewed_pr(10, "lower-branch", "APPROVED")];

        assert!(matches!(
            overlay(&[&local, &lower], &prs),
            WorkState::Dirty { .. }
        ));
    }

    #[test]
    fn overlay_prefers_pushed_change_over_lower_open_pr() {
        let pushed = cc("pushed", false, &["pushed-branch"], true, true);
        let lower = cc("lower", false, &["lower-branch"], true, false);
        let prs = [reviewed_pr(10, "lower-branch", "APPROVED")];

        assert_eq!(overlay(&[&pushed, &lower], &prs), WorkState::Pushed);
    }

    #[test]
    fn lower_changes_requested_pr_is_not_masked_by_stacked_work() {
        // The one exception to "least delivered wins" (ticket 31, ADR 0003): a
        // change-requested PR lower in the stack is blocked on the human, so the
        // nudge must resurface even while newer work is stacked above it. The row
        // must read `PrOpen { ChangesRequested }`, not "ready to forge".
        let local = cc("local", false, &[], false, true);
        let pushed = cc("pushed", false, &["pushed-branch"], true, true);
        let lower = cc("lower", false, &["lower-branch"], true, false);
        let prs = [reviewed_pr(10, "lower-branch", "CHANGES_REQUESTED")];
        let changes = WorkState::PrOpen {
            number: 10,
            verdict: ReviewVerdict::ChangesRequested,
        };

        // A newer non-empty local commit above the blocked PR.
        assert_eq!(overlay(&[&local, &lower], &prs), changes);
        // A newer pushed commit above the blocked PR.
        assert_eq!(overlay(&[&pushed, &lower], &prs), changes);
        // The combined signal: a Waiting agent over that stack is needs-you.
        assert_eq!(
            crate::attention::derive(
                crate::agent::AgentState::Waiting,
                changes,
                crate::engine::RunHealth::Live
            ),
            crate::attention::Attention::NeedsYou
        );
    }

    #[test]
    fn overlay_uses_lowest_pr_with_the_most_blocking_verdict() {
        let top = cc("top", false, &["top-branch"], true, true);
        let middle = cc("middle", false, &["middle-branch"], true, false);
        let bottom = cc("bottom", false, &["bottom-branch"], true, false);
        let prs = [
            reviewed_pr(10, "bottom-branch", "APPROVED"),
            reviewed_pr(30, "top-branch", "CHANGES_REQUESTED"),
            reviewed_pr(20, "middle-branch", "CHANGES_REQUESTED"),
        ];

        assert_eq!(
            overlay(&[&top, &middle, &bottom], &prs),
            WorkState::PrOpen {
                number: 20,
                verdict: ReviewVerdict::ChangesRequested,
            }
        );
    }

    #[test]
    fn overlay_orders_all_open_review_verdicts_by_blocking_priority() {
        let top = cc("top", false, &["top-branch"], true, true);
        let middle = cc("middle", false, &["middle-branch"], true, false);
        let bottom = cc("bottom", false, &["bottom-branch"], true, false);
        let review_required = [
            reviewed_pr(30, "top-branch", "APPROVED"),
            pr(20, "middle-branch", "OPEN", None),
            reviewed_pr(10, "bottom-branch", "REVIEW_REQUIRED"),
        ];

        assert_eq!(
            overlay(&[&top, &middle, &bottom], &review_required),
            WorkState::PrOpen {
                number: 10,
                verdict: ReviewVerdict::ReviewRequired,
            }
        );

        let no_decision = [
            reviewed_pr(30, "top-branch", "APPROVED"),
            pr(20, "middle-branch", "OPEN", None),
        ];
        assert_eq!(
            overlay(&[&top, &middle], &no_decision),
            WorkState::PrOpen {
                number: 20,
                verdict: ReviewVerdict::None,
            }
        );
    }

    /// A read port programmed with the jj/gh facts one scenario needs, so the
    /// classification runs through the same interface production does.
    #[derive(Default)]
    struct FakeWorkReads {
        prs: Vec<crate::prs::Pr>,
        /// When set, the Pull Request read fails instead of returning `prs`.
        prs_failed: bool,
        chains: HashMap<String, Option<Chain>>,
        behind: HashMap<String, u32>,
        /// When set, the behind read fails instead of returning a count.
        behind_failed: bool,
        deltas: HashMap<String, (u32, u32)>,
        /// When set, `line_delta` answers only for this base change id.
        delta_base: Option<String>,
        /// When set, the diff read fails instead of returning a delta.
        delta_failed: bool,
    }

    impl WorkReads for FakeWorkReads {
        fn pull_requests(&self) -> Result<Vec<crate::prs::Pr>, String> {
            if self.prs_failed {
                return Err("gh pr list: not authenticated".to_owned());
            }
            Ok(self.prs.clone())
        }

        fn chain(&self, workspace: &str) -> Option<Chain> {
            self.chains.get(workspace).cloned().unwrap_or(None)
        }

        fn behind(&self, workspace: &str) -> Result<u32, String> {
            if self.behind_failed {
                return Err("jj log: boom".to_owned());
            }
            Ok(self.behind.get(workspace).copied().unwrap_or(0))
        }

        fn line_delta(&self, workspace: &str, base: Option<&str>) -> Result<(u32, u32), String> {
            if self.delta_failed {
                return Err("jj diff: boom".to_owned());
            }
            if self.delta_base.as_deref() != base {
                return Ok((0, 0));
            }
            Ok(self.deltas.get(workspace).copied().unwrap_or((0, 0)))
        }
    }

    fn chain(commits: Vec<ChainCommit>) -> Option<Chain> {
        Some(Chain { commits })
    }

    #[test]
    fn snapshot_classifies_every_workspace_through_the_read_port() {
        let fake = FakeWorkReads {
            chains: HashMap::from([
                (
                    "clean".to_string(),
                    chain(vec![cc("c", true, &[], false, true)]),
                ),
                (
                    "dirty".to_string(),
                    chain(vec![cc("d", false, &[], false, true)]),
                ),
            ]),
            behind: HashMap::from([("dirty".to_string(), 4)]),
            deltas: HashMap::from([("dirty".to_string(), (3, 1))]),
            delta_base: Some("d".to_string()),
            ..FakeWorkReads::default()
        };

        let work = snapshot(&fake, &["clean".to_string(), "dirty".to_string()]);

        assert_eq!(
            work["clean"],
            Work {
                state: WorkState::Clean,
                behind: 0,
            }
        );
        assert_eq!(
            work["dirty"],
            Work {
                state: WorkState::Dirty {
                    added: 3,
                    removed: 1,
                },
                behind: 4,
            }
        );
    }

    #[test]
    fn snapshot_degrades_to_unknown_when_a_chain_read_fails() {
        let fake = FakeWorkReads::default();
        let work = snapshot(&fake, &["gone".to_string()]);
        assert_eq!(work["gone"].state, WorkState::Unknown);
    }

    #[test]
    fn a_failed_behind_read_is_unknown_not_zero_behind() {
        // A failed behind read must not claim "0 behind" - that is the silent
        // zero this ticket removes. The row degrades to unknown instead.
        let fake = FakeWorkReads {
            chains: HashMap::from([(
                "feat".to_string(),
                chain(vec![cc("tip", false, &[], false, true)]),
            )]),
            behind_failed: true,
            ..FakeWorkReads::default()
        };

        assert_eq!(
            snapshot(&fake, &["feat".to_string()])["feat"],
            Work {
                state: WorkState::Unknown,
                behind: 0,
            }
        );
    }

    #[test]
    fn snapshot_matches_an_open_pull_request_by_bookmark_through_the_read_port() {
        let fake = FakeWorkReads {
            prs: vec![reviewed_pr(12, "feat-br", "APPROVED")],
            chains: HashMap::from([(
                "feat".to_string(),
                chain(vec![cc("tip", false, &["feat-br"], true, true)]),
            )]),
            ..FakeWorkReads::default()
        };

        let work = snapshot(&fake, &["feat".to_string()]);

        assert_eq!(
            work["feat"].state,
            WorkState::PrOpen {
                number: 12,
                verdict: ReviewVerdict::Approved,
            }
        );
    }

    #[test]
    fn snapshot_measures_the_line_delta_from_the_deepest_owned_commit() {
        let fake = FakeWorkReads {
            chains: HashMap::from([(
                "feat".to_string(),
                chain(vec![
                    cc("tip", false, &[], false, true),
                    cc("base", false, &[], false, true),
                ]),
            )]),
            deltas: HashMap::from([("feat".to_string(), (7, 2))]),
            delta_base: Some("base".to_string()),
            ..FakeWorkReads::default()
        };

        let work = snapshot(&fake, &["feat".to_string()]);

        assert_eq!(
            work["feat"].state,
            WorkState::Dirty {
                added: 7,
                removed: 2,
            }
        );
    }

    #[test]
    fn a_failed_diff_read_is_unknown_not_a_zero_delta() {
        // A failed diff read must not claim "dirty +0/-0": that is the silent
        // zero this ticket removes. The state degrades to unknown instead.
        let fake = FakeWorkReads {
            chains: HashMap::from([(
                "feat".to_string(),
                chain(vec![cc("tip", false, &[], false, true)]),
            )]),
            delta_failed: true,
            ..FakeWorkReads::default()
        };

        assert_eq!(
            snapshot(&fake, &["feat".to_string()])["feat"].state,
            WorkState::Unknown
        );
    }

    #[test]
    fn a_failed_pull_request_read_is_unknown_not_pushed() {
        // A pushed commit with no Pull Request read might already be open or
        // merged. Reporting `Pushed` would claim it still needs a forge.
        let pushed = || FakeWorkReads {
            chains: HashMap::from([(
                "feat".to_string(),
                chain(vec![cc("tip", false, &["other"], true, true)]),
            )]),
            ..FakeWorkReads::default()
        };

        // No Pull Requests, read successfully: genuinely still pushed.
        assert_eq!(
            snapshot(&pushed(), &["feat".to_string()])["feat"].state,
            WorkState::Pushed
        );

        // The read failed: the same chain must not claim `Pushed`.
        let failed = FakeWorkReads {
            prs_failed: true,
            ..pushed()
        };
        assert_eq!(
            snapshot(&failed, &["feat".to_string()])["feat"].state,
            WorkState::Unknown
        );
    }

    #[test]
    fn a_failed_pull_request_read_keeps_a_local_dirty_state() {
        // A local-only change is `Dirty` with or without Pull Request knowledge,
        // so a failed read must not throw that away.
        let fake = FakeWorkReads {
            prs_failed: true,
            chains: HashMap::from([(
                "feat".to_string(),
                chain(vec![cc("tip", false, &[], false, true)]),
            )]),
            ..FakeWorkReads::default()
        };

        let work = snapshot(&fake, &["feat".to_string()]);

        assert!(matches!(work["feat"].state, WorkState::Dirty { .. }));
    }

    /// A throwaway non-colocated jj repository for the real-jj read tests.
    fn temp_repo(tag: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the epoch")
            .as_nanos();
        let repo =
            std::env::temp_dir().join(format!("jjfx-work-{tag}-{}-{nonce}", std::process::id()));
        let status = std::process::Command::new("jj")
            .args([
                "--config",
                "signing.behavior=drop",
                "git",
                "init",
                "--no-colocate",
            ])
            .arg(&repo)
            .status()
            .expect("jj should be installed");
        assert!(status.success(), "jj git init should succeed");
        repo
    }

    /// Add a Workspace named `name` to `root`, returning its directory. The
    /// directory is scoped to the repository so concurrent tests cannot collide.
    fn add_workspace(root: &Path, name: &str) -> std::path::PathBuf {
        let path = root
            .parent()
            .expect("temporary repository should have a parent")
            .join(format!(
                "{}-{}",
                root.file_name()
                    .expect("temporary repository should have a name")
                    .to_string_lossy(),
                name.replace([' ', '(', ')'], "_")
            ));
        let status = std::process::Command::new("jj")
            .args([
                "--config",
                "signing.behavior=drop",
                "workspace",
                "add",
                "--name",
                name,
                "--no-colocate",
            ])
            .arg(&path)
            .current_dir(root)
            .status()
            .expect("jj workspace add should run");
        assert!(status.success(), "jj workspace add should succeed");
        path
    }

    #[test]
    fn workspace_names_with_spaces_and_parens_still_read_their_chain() {
        // jj rejects `::my work@` and `::a(b)@` as revsets, so an unquoted name
        // used to make the chain read fail and the Workspace read `Unknown`.
        let repo = temp_repo("chain");
        let root = repo.canonicalize().expect("temporary repo canonicalizes");
        let reads = SystemWorkReads::new(root.clone());
        let created: Vec<_> = ["my work", "a(b)"]
            .into_iter()
            .map(|name| {
                let path = add_workspace(&root, name);
                assert!(
                    reads.chain(name).is_some(),
                    "workspace {name:?} must still resolve its chain"
                );
                path
            })
            .collect();

        for path in created {
            std::fs::remove_dir_all(&path).ok();
        }
        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn a_space_named_workspace_is_not_unknown_through_the_read_port() {
        // The shape the work poller reads: one Workspace whose name needs
        // quoting must classify, not degrade to `Unknown`.
        let repo = temp_repo("snapshot");
        let root = repo.canonicalize().expect("temporary repo canonicalizes");
        let workspace = add_workspace(&root, "my work");

        let work = snapshot(
            &SystemWorkReads::new(root.clone()),
            &["my work".to_string()],
        );

        assert_ne!(
            work["my work"].state,
            WorkState::Unknown,
            "a workspace name with a space must not read as Unknown"
        );

        std::fs::remove_dir_all(&workspace).ok();
        std::fs::remove_dir_all(&repo).ok();
    }
}
