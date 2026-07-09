//! One model of "a pull request as `gh` reports it", and the `gh pr list` reads
//! jjfx does over it. Both consumers - `work`'s lifecycle overlay and `forge`'s
//! stacked-PR submission ([`crate::pr`]) - project from the one [`Pr`] here, so
//! the "is this PR merged?" question is answered in exactly one place and the two
//! callers cannot drift apart (the divergence this module exists to remove).

use serde::Deserialize;

use crate::cmd::cmd;

/// The `--json` fields every `gh pr list` read requests, spelled once. Their
/// camelCase spellings map onto [`Pr`]'s fields (two via `serde(rename)`).
const FIELDS: &str = "number,headRefName,state,reviewDecision,body,mergedAt";

/// One PR as reported by `gh pr list --json`. Carries the union of the fields the
/// two consumers need; each reads only the subset it cares about.
#[derive(Debug, Clone, Deserialize)]
pub struct Pr {
    pub number: u64,
    /// The PR's head branch (`headRefName`), matched against a workspace's local
    /// bookmarks to associate a PR with a workspace.
    #[serde(rename = "headRefName")]
    pub head: String,
    /// GraphQL state: `OPEN` | `CLOSED` | `MERGED`.
    pub state: String,
    /// The review verdict (`reviewDecision`); `None` when no decision yet.
    #[serde(rename = "reviewDecision")]
    pub review: Option<String>,
    /// The PR description; rewritten with a `## Stack` section on submission.
    pub body: Option<String>,
    #[serde(rename = "mergedAt")]
    pub merged_at: Option<String>,
}

impl Pr {
    /// Whether this PR has merged. `gh` reports it two ways that must agree - the
    /// GraphQL `state` of `MERGED`, and a non-null `mergedAt` timestamp - so treat
    /// either as merged. Stating it once here is why no two callers can disagree.
    pub fn is_merged(&self) -> bool {
        self.state == "MERGED" || self.merged_at.is_some()
    }
}

/// Run one `gh pr list` over `slug` with the shared [`FIELDS`] set plus a
/// per-call `filter` (limit, and optionally `--head`), parsed into [`Pr`]s. The
/// single home for the `gh pr list` incantation. `Err` on a spawn failure, a
/// non-zero exit (carrying gh's stderr), or unparseable JSON.
fn query(slug: &str, filter: &[&str]) -> Result<Vec<Pr>, String> {
    let mut args = vec!["pr", "list", "-R", slug, "--state", "all", "--json", FIELDS];
    args.extend_from_slice(filter);
    let out = cmd("gh").args(args).run().map_err(|e| e.to_string())?;
    if !out.ok() {
        return Err(format!("gh pr list: {}", out.stderr().trim()));
    }
    serde_json::from_str(out.stdout()).map_err(|e| e.to_string())
}

/// Every PR in the repo (all states, up to 100), for overlaying work state onto
/// the workspace list. Returns an empty list on any failure, so a missing `gh`,
/// no auth, or no network degrades to "no PR info" rather than crashing.
pub fn list(slug: &str) -> Vec<Pr> {
    query(slug, &["--limit", "100"]).unwrap_or_default()
}

/// The PR for one branch: an open one wins, else a merged one; closed-unmerged
/// and absent both yield `None`. Surfaces `gh` failures as an error string (the
/// forge submission path shows them), unlike [`list`], which degrades silently.
pub fn find(slug: &str, branch: &str) -> Result<Option<Pr>, String> {
    let prs = query(slug, &["--head", branch, "--limit", "20"])?;
    Ok(pick(prs))
}

/// Choose the one relevant PR for a branch from a `gh` result: an open PR wins,
/// else the first merged one; closed-unmerged and empty both yield `None`.
fn pick(prs: Vec<Pr>) -> Option<Pr> {
    let open = prs.iter().find(|p| p.state == "OPEN").cloned();
    open.or_else(|| prs.into_iter().find(Pr::is_merged))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(number: u64, state: &str, merged_at: Option<&str>) -> Pr {
        Pr {
            number,
            head: "branch".to_string(),
            state: state.to_string(),
            review: None,
            body: None,
            merged_at: merged_at.map(String::from),
        }
    }

    #[test]
    fn deserializes_the_union_of_gh_json_fields() {
        // A representative `gh pr list --json` row: the camelCase keys gh emits
        // must map onto the model's fields, including the two renames.
        let json = r#"[{
            "number": 42,
            "headRefName": "adam/feature",
            "state": "OPEN",
            "reviewDecision": "APPROVED",
            "body": "the description",
            "mergedAt": null
        }]"#;
        let prs: Vec<Pr> = serde_json::from_str(json).expect("gh json parses");
        let pr = &prs[0];
        assert_eq!(pr.number, 42);
        assert_eq!(pr.head, "adam/feature");
        assert_eq!(pr.state, "OPEN");
        assert_eq!(pr.review.as_deref(), Some("APPROVED"));
        assert_eq!(pr.body.as_deref(), Some("the description"));
        assert_eq!(pr.merged_at, None);
    }

    #[test]
    fn is_merged_accepts_either_gh_signal() {
        // gh's two mergedness signals, and the negative case, as independent
        // known-good inputs: state alone, mergedAt alone, and neither.
        assert!(pr(1, "MERGED", None).is_merged());
        assert!(pr(1, "CLOSED", Some("2026-01-01T00:00:00Z")).is_merged());
        assert!(!pr(1, "OPEN", None).is_merged());
        assert!(!pr(1, "CLOSED", None).is_merged());
    }

    #[test]
    fn pick_prefers_open_then_merged_else_none() {
        // An open PR wins even when a merged one is also present.
        let both = vec![
            pr(1, "MERGED", Some("2026-01-01T00:00:00Z")),
            pr(2, "OPEN", None),
        ];
        assert_eq!(pick(both).map(|p| p.number), Some(2));
        // No open one: the merged one is chosen.
        let merged = vec![pr(3, "CLOSED", None), pr(4, "MERGED", None)];
        assert_eq!(pick(merged).map(|p| p.number), Some(4));
        // Only closed-unmerged: nothing to report.
        assert_eq!(pick(vec![pr(5, "CLOSED", None)]).map(|p| p.number), None);
        // Empty.
        assert_eq!(pick(Vec::new()).map(|p| p.number), None);
    }
}
