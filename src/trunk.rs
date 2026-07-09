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

#[cfg(test)]
mod tests {
    use super::*;

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
}
