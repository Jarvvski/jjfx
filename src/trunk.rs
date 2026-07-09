//! The trunk-selection rule, stated once (ADR-0007).
//!
//! "Which commit is trunk?" is needed on two paths that run on different engines:
//! the CLI reads (`work`, `diff`, `pr`, `jj`) consume a revset string, while the
//! graph ([`crate::graph`]) resolves a concrete [`CommitId`] by walking the store
//! through jj-lib. Both derive from the one ordered [`SOURCES`] list here, so the
//! revset and the walk cannot drift out of agreement.
//!
//! `forge`'s push/weld target is a *deliberate* third notion (jj's bare `trunk()`,
//! the real-remote mainline) and is documented as an exception at its call site,
//! not routed through here.

use std::sync::Arc;

use jj_lib::backend::CommitId;
use jj_lib::ref_name::{RefName, RemoteName};
use jj_lib::store::Store;
use jj_lib::view::View;

/// One trunk candidate, in priority order. The most recent that resolves wins
/// (revset `latest(...)` / [`pick_trunk`]'s max-by-timestamp); the repo root is
/// the fallback when none resolve.
///
/// Most-recent (not remote-first) is the point: a local `main` ahead of
/// `origin/main` must win, else every unpushed mainline commit would show as
/// workspace divergence below trunk. The root fallback keeps a never-pushed repo
/// (no bookmark) from treating all history as one branch (v0.8.1).
#[derive(Debug, Clone, Copy)]
enum Source {
    /// jj's `trunk()` minus the root - the real-remote mainline, i.e. `origin`'s
    /// `main`/`master`. Only the `origin` remote counts as "really pushed".
    RemoteMainline,
    LocalMain,
    LocalMaster,
    LocalTrunk,
}

/// The trunk candidates, highest priority first. This is the single statement of
/// the rule; both [`as_revset`] and [`resolve`] iterate it.
const SOURCES: [Source; 4] = [
    Source::RemoteMainline,
    Source::LocalMain,
    Source::LocalMaster,
    Source::LocalTrunk,
];

impl Source {
    /// This source as a revset fragment. `present(...)` stops a missing local
    /// bookmark from erroring the whole revset.
    fn revset_fragment(self) -> &'static str {
        match self {
            Source::RemoteMainline => "(trunk() ~ root())",
            Source::LocalMain => "present(main)",
            Source::LocalMaster => "present(master)",
            Source::LocalTrunk => "present(trunk)",
        }
    }

    /// This source resolved to concrete commit ids, in priority order. The
    /// real-remote mainline contributes `origin`'s `main` then `master`; each
    /// local source is a single bookmark. A source that does not resolve yields
    /// nothing (the jj-lib analogue of the revset's `present(...)`).
    fn lookup<Id>(
        self,
        remote: &impl Fn(&str) -> Option<Id>,
        local: &impl Fn(&str) -> Option<Id>,
    ) -> Vec<Id> {
        match self {
            Source::RemoteMainline => [remote("main"), remote("master")]
                .into_iter()
                .flatten()
                .collect(),
            Source::LocalMain => local("main").into_iter().collect(),
            Source::LocalMaster => local("master").into_iter().collect(),
            Source::LocalTrunk => local("trunk").into_iter().collect(),
        }
    }
}

/// The mainline base a workspace's own work is measured against, as a revset for
/// the CLI reads. Latest of the real-remote mainline and the local
/// `main`/`master`/`trunk` bookmarks, else the root.
pub(crate) fn as_revset() -> String {
    let union = SOURCES
        .iter()
        .map(|s| s.revset_fragment())
        .collect::<Vec<_>>()
        .join(" | ");
    format!("latest({union})")
}

/// The trunk candidate commit ids, highest-priority first, gathered from
/// [`SOURCES`]. Generic over the id type so it is unit-testable with plain
/// closures instead of a live jj-lib store.
fn candidate_ids<Id>(
    remote: &impl Fn(&str) -> Option<Id>,
    local: &impl Fn(&str) -> Option<Id>,
) -> Vec<Id> {
    SOURCES
        .iter()
        .flat_map(|s| s.lookup(remote, local))
        .collect()
}

/// Resolve the trunk commit through jj-lib, the graph's adapter for the same rule
/// [`as_revset`] states for the CLI. Reads the `origin` remote and local
/// bookmarks off `view`, then takes the most recent by committer timestamp
/// (mirroring the revset's `latest(...)`), else the repo `root`. Only `origin`
/// counts as "really pushed": jj's colocated `git` remote is not queried, so a
/// git-tracked-but-unpushed bookmark cannot masquerade as trunk.
pub(crate) fn resolve(view: &View, store: &Arc<Store>, root: CommitId) -> CommitId {
    let remote = |name: &str| -> Option<CommitId> {
        view.remote_bookmarks(RemoteName::new("origin"))
            .find(|(n, _)| n.as_str() == name)
            .and_then(|(_, r)| r.target.as_normal().cloned())
    };
    let local = |name: &str| -> Option<CommitId> {
        view.get_local_bookmark(RefName::new(name))
            .as_normal()
            .cloned()
    };
    let candidates: Vec<(CommitId, i64)> = candidate_ids(&remote, &local)
        .into_iter()
        .filter_map(|id| {
            store
                .get_commit(&id)
                .ok()
                .map(|c| (id, c.committer().timestamp.timestamp.0))
        })
        .collect();
    pick_trunk(&candidates, root)
}

/// Pure trunk pick: the candidate with the most recent timestamp, else the
/// fallback. Ties resolve to the last max, which is fine - tied candidates point
/// at the same commit (e.g. `origin/main` == local `main` just after a push).
fn pick_trunk<T: Clone>(candidates: &[(T, i64)], fallback: T) -> T {
    candidates
        .iter()
        .max_by_key(|(_, ts)| *ts)
        .map(|(id, _)| id.clone())
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_ids_follow_source_priority_and_skip_missing() {
        // Every source resolves: order is real-remote main, real-remote master,
        // then local main/master/trunk - the same priority the revset union
        // encodes, so the two adapters agree by construction.
        let all = candidate_ids(&|n: &str| Some(format!("remote/{n}")), &|n: &str| {
            Some(format!("local/{n}"))
        });
        assert_eq!(
            all,
            [
                "remote/main",
                "remote/master",
                "local/main",
                "local/master",
                "local/trunk"
            ]
        );

        // A source that does not resolve is skipped; the rest keep their order.
        let no_remote = candidate_ids(&|_: &str| None::<String>, &|n: &str| {
            Some(format!("local/{n}"))
        });
        assert_eq!(no_remote, ["local/main", "local/master", "local/trunk"]);
    }

    #[test]
    fn as_revset_states_the_rule_as_one_latest_union() {
        // The canonical base revset, as an independent known-good literal (the
        // string the CLI reads have always used). Built from SOURCES, it must
        // reproduce this exactly.
        assert_eq!(
            as_revset(),
            "latest((trunk() ~ root()) | present(main) | present(master) | present(trunk))"
        );
    }

    #[test]
    fn pick_trunk_picks_the_most_recent_candidate() {
        // A local `main` ahead of `origin/main` must win (matches `latest(...)`),
        // else unpushed mainline commits would show as workspace divergence.
        let candidates = [("origin-main", 100_i64), ("local-main", 200_i64)];
        assert_eq!(pick_trunk(&candidates, "root"), "local-main");
    }

    #[test]
    fn pick_trunk_falls_back_to_root_when_no_candidates() {
        // The never-pushed / no-bookmark case (v0.8.1): nothing resolves, so the
        // caller must land on the root commit, not error.
        let none: [(&str, i64); 0] = [];
        assert_eq!(pick_trunk(&none, "root"), "root");
    }
}
