//! The commit graph, read from jj-lib (ADR 0007).
//!
//! Unlike the other jj reads (which still shell to the CLI), the graph opens the
//! on-disk `.jj/` store directly through jj-lib and walks the DAG by commit id -
//! the pattern spike 02 proved. Nothing here scrapes `jj log --color always`; the
//! layout is built by us from typed commit data.
//!
//! jj-lib has no stability guarantee and reads the store format of the installed
//! `jj`, so its version is locked to the `jj` mise pin (0.43.0) and the two move
//! in lockstep. The surface used here is deliberately minimal - open, read heads
//! and bookmarks, walk parents - to keep the migration cost of a `jj` bump small.
//!
//! The build splits into a thin jj-lib I/O shell ([`load`]) and a pure core
//! ([`pick_trunk`], [`build`]) that is unit-tested without a real store.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::{Context, anyhow};
use jj_lib::backend::CommitId;
use jj_lib::config::StackedConfig;
use jj_lib::object_id::ObjectId;
use jj_lib::ref_name::{RefName, RemoteName};
use jj_lib::repo::{Repo, StoreFactories};
use jj_lib::settings::UserSettings;
use jj_lib::workspace::{Workspace, default_working_copy_factories};

/// One commit in the graph, in display form (owned strings, no jj-lib types leak
/// out of this module).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Full commit-id hex (the stable key used by chains and adjacency).
    pub id: String,
    /// Short change id in jj's reverse-hex letters (what `jj log` shows).
    pub change_id: String,
    /// First line of the description, or a placeholder when empty.
    pub summary: String,
    /// Parent commit-id hexes (first parent first).
    pub parents: Vec<String>,
    /// Local bookmark names pointing at this commit.
    pub bookmarks: Vec<String>,
    /// Author timestamp in millis since epoch, for freshness shading.
    pub timestamp_ms: i64,
    /// Workspace names whose working-copy `@` is this commit (usually 0 or 1).
    pub wc_of: Vec<String>,
}

/// One workspace's chain: its own commits above `trunk()`, the trunk commit it
/// attaches to, and the one child past `@` (the "n+1").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    pub workspace: String,
    /// The working-copy `@` commit id (may equal `base` for a clean workspace
    /// sitting on the trunk tip, in which case `commits` is empty).
    pub head: String,
    /// Trunk-exclusive commits, ordered `@` (or nearest) down to the branch
    /// point. Empty when the workspace sits directly on trunk.
    pub commits: Vec<String>,
    /// The trunk commit this chain branches from, if it was reached.
    pub base: Option<String>,
    /// One child of `@`, if `@` is not itself a leaf (the "n+1" context commit).
    pub child: Option<String>,
}

/// The assembled graph: a node lookup plus per-workspace chains and the trunk
/// spine. Views index into `nodes` by id.
#[derive(Debug, Clone, Default)]
pub struct Graph {
    pub nodes: BTreeMap<String, Node>,
    /// Trunk tip commit id, if one resolved.
    pub trunk_id: Option<String>,
    /// Trunk tip then a few ancestors, for a little mainline context in the
    /// world view (newest first).
    pub trunk_context: Vec<String>,
    /// One chain per workspace, `default` first then alphabetical.
    pub chains: Vec<Chain>,
}

impl Graph {
    /// The chain for a named workspace, if present.
    pub fn chain(&self, workspace: &str) -> Option<&Chain> {
        self.chains.iter().find(|c| c.workspace == workspace)
    }
}

/// Open the repo through jj-lib and assemble its [`Graph`]. Blocking (it
/// `block_on`s jj-lib's async loader), so callers run it on `spawn_blocking`,
/// never the render task.
pub fn load(repo_root: &Path) -> anyhow::Result<Graph> {
    let settings =
        UserSettings::from_config(StackedConfig::with_defaults()).context("build jj settings")?;
    let workspace = Workspace::load(
        &settings,
        repo_root,
        &StoreFactories::default(),
        &default_working_copy_factories(),
    )
    .context("open jj workspace")?;

    // `load_at_head` is async in jj-lib 0.43; block on it - this whole fn runs on
    // a blocking worker thread (spike 02).
    let repo = pollster::block_on(workspace.repo_loader().load_at_head())
        .map_err(|e| anyhow!("load repo at head: {e}"))?;
    let view = repo.view();
    let store = repo.store();
    let root = store.root_commit_id().clone();

    // Workspace name -> its `@` commit id.
    let wc: BTreeMap<String, CommitId> = view
        .wc_commit_ids()
        .iter()
        .map(|(name, id)| (name.as_str().to_string(), id.clone()))
        .collect();

    // Local bookmark names by commit, and the trunk candidates.
    let mut bookmarks: HashMap<CommitId, Vec<String>> = HashMap::new();
    for (name, target) in view.local_bookmarks() {
        if let Some(id) = target.as_normal() {
            bookmarks
                .entry(id.clone())
                .or_default()
                .push(name.as_str().to_string());
        }
    }

    let trunk_id = resolve_trunk(view, store, root.clone());

    // Phase A: trunk ancestors (bounded), so we can tell where a branch rejoins
    // the mainline without walking all of history. Also captures trunk context.
    let mut loaded: HashMap<CommitId, jj_lib::commit::Commit> = HashMap::new();
    let mut trunk_ancestors: HashSet<CommitId> = HashSet::new();
    let mut trunk_context: Vec<CommitId> = Vec::new();
    let mut queue: VecDeque<CommitId> = VecDeque::new();
    queue.push_back(trunk_id.clone());
    while let Some(id) = queue.pop_front() {
        if trunk_ancestors.contains(&id) || trunk_ancestors.len() >= TRUNK_ANCESTOR_CAP {
            continue;
        }
        let commit = match store.get_commit(&id) {
            Ok(c) => c,
            Err(_) => continue,
        };
        trunk_ancestors.insert(id.clone());
        if trunk_context.len() < TRUNK_CONTEXT {
            trunk_context.push(id.clone());
        }
        for p in commit.parent_ids() {
            queue.push_back(p.clone());
        }
        loaded.insert(id, commit);
    }

    // Phase B: the workspace neighbourhood. Seed with every head and every `@`
    // (heads capture children of `@`); walk parents but stop at the trunk
    // boundary so we never pull in deep mainline history.
    let mut seeds: Vec<CommitId> = view.heads().iter().cloned().collect();
    seeds.extend(wc.values().cloned());
    let mut expanded: HashSet<CommitId> = HashSet::new();
    let mut queue: VecDeque<CommitId> = seeds.into_iter().collect();
    while let Some(id) = queue.pop_front() {
        if !expanded.insert(id.clone()) || loaded.len() >= NEIGHBOURHOOD_CAP {
            continue;
        }
        let parents = match loaded.get(&id) {
            Some(c) => c.parent_ids().to_vec(),
            None => match store.get_commit(&id) {
                Ok(c) => {
                    let parents = c.parent_ids().to_vec();
                    loaded.insert(id.clone(), c);
                    parents
                }
                Err(_) => continue,
            },
        };
        // Boundary: a commit on the trunk line is an anchor, not something to
        // expand through.
        if trunk_ancestors.contains(&id) {
            continue;
        }
        for p in parents {
            queue.push_back(p);
        }
    }

    Ok(build(
        &loaded,
        &bookmarks,
        &wc,
        &trunk_ancestors,
        &trunk_context,
        &trunk_id,
    ))
}

/// Depth caps: bounded so a huge repo never stalls the read. A workspace's branch
/// point is normally a handful of commits back; these are generous safety nets.
const TRUNK_ANCESTOR_CAP: usize = 1000;
const TRUNK_CONTEXT: usize = 6;
const NEIGHBOURHOOD_CAP: usize = 3000;
/// Longest single workspace chain we will render before giving up (graceful).
const CHAIN_CAP: usize = 500;

/// Resolve the trunk commit the way `work::TRUNK_BASE` does, so the graph agrees
/// with the workspace list's clean/dirty/behind. That revset,
/// `latest((trunk() ~ root()) | present(main) | present(master) | present(trunk))`,
/// takes the *most recent* of the real-remote mainline and the local `main`/
/// `master`/`trunk` bookmarks, else the root. Most-recent (not remote-first) is the
/// point: a local `main` ahead of `origin/main` must win, or every unpushed
/// mainline commit would show as workspace divergence below trunk. The root
/// fallback keeps a never-pushed repo (no bookmark) from treating all history as
/// one branch (v0.8.1).
fn resolve_trunk(
    view: &jj_lib::view::View,
    store: &std::sync::Arc<jj_lib::store::Store>,
    root: CommitId,
) -> CommitId {
    // Only the `origin` remote counts as "really pushed"; jj's colocated `git`
    // remote is not queried, so a git-tracked-but-unpushed bookmark cannot
    // masquerade as trunk.
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
    // Each candidate paired with its committer timestamp, so the latest wins
    // (mirroring the revset's `latest(...)`).
    let candidates: Vec<(CommitId, i64)> = [
        remote("main"),
        remote("master"),
        local("main"),
        local("master"),
        local("trunk"),
    ]
    .into_iter()
    .flatten()
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

/// Assemble the graph from loaded commits. Projects the jj-lib `Commit` map into
/// plain string maps up front, then delegates the chain/trunk-boundary logic to
/// the pure [`build_chains`] so that logic is unit-testable without a real store.
fn build(
    loaded: &HashMap<CommitId, jj_lib::commit::Commit>,
    bookmarks: &HashMap<CommitId, Vec<String>>,
    wc: &BTreeMap<String, CommitId>,
    trunk_ancestors: &HashSet<CommitId>,
    trunk_context: &[CommitId],
    trunk_id: &CommitId,
) -> Graph {
    let wc_of: HashMap<String, Vec<String>> = {
        let mut m: HashMap<String, Vec<String>> = HashMap::new();
        for (name, id) in wc {
            if loaded.contains_key(id) {
                m.entry(id.hex()).or_default().push(name.clone());
            }
        }
        for v in m.values_mut() {
            v.sort();
        }
        m
    };

    let nodes: BTreeMap<String, Node> = loaded
        .iter()
        .map(|(id, commit)| {
            let hex = id.hex();
            let node = Node {
                change_id: display_change_id(&commit.change_id().hex(), 8),
                summary: summary(commit.description()),
                parents: commit.parent_ids().iter().map(ObjectId::hex).collect(),
                bookmarks: bookmarks.get(id).cloned().unwrap_or_default(),
                timestamp_ms: commit.author().timestamp.timestamp.0,
                wc_of: wc_of.get(&hex).cloned().unwrap_or_default(),
                id: hex.clone(),
            };
            (hex, node)
        })
        .collect();

    let trunk_ancestors: HashSet<String> = trunk_ancestors.iter().map(ObjectId::hex).collect();
    let wc_hex: BTreeMap<String, String> = wc.iter().map(|(n, id)| (n.clone(), id.hex())).collect();
    let chains = build_chains(&nodes, &wc_hex, &trunk_ancestors);

    Graph {
        trunk_id: loaded.contains_key(trunk_id).then(|| trunk_id.hex()),
        trunk_context: trunk_context.iter().map(ObjectId::hex).collect(),
        chains,
        nodes,
    }
}

/// Pure per-workspace chain derivation over the string node map. Each chain is a
/// first-parent walk from `@` down to the trunk boundary, plus the one child past
/// `@`. First-parent keeps the chain linear; merges (rare in these workspaces)
/// show only their mainline side, which is acceptable for v1.
fn build_chains(
    nodes: &BTreeMap<String, Node>,
    wc: &BTreeMap<String, String>,
    trunk_ancestors: &HashSet<String>,
) -> Vec<Chain> {
    // Reverse adjacency, to find the child past `@`.
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for node in nodes.values() {
        for p in &node.parents {
            children.entry(p.as_str()).or_default().push(&node.id);
        }
    }

    let chains = wc
        .iter()
        .map(|(workspace, at)| {
            let mut commits = Vec::new();
            let mut cur = at.clone();
            let mut depth = 0;
            while !trunk_ancestors.contains(&cur) && depth < CHAIN_CAP {
                commits.push(cur.clone());
                let Some(parent) = nodes.get(&cur).and_then(|n| n.parents.first()).cloned() else {
                    break;
                };
                cur = parent;
                depth += 1;
            }
            let base = trunk_ancestors.contains(&cur).then(|| cur.clone());

            // The "n+1": the child of `@` (deterministic by id), only meaningful
            // when `@` is not a leaf - e.g. after `jj edit`ing into history.
            let child = children
                .get(at.as_str())
                .and_then(|kids| kids.iter().min())
                .map(|id| id.to_string());

            Chain {
                workspace: workspace.clone(),
                head: at.clone(),
                commits,
                base,
                child,
            }
        })
        .collect();

    order_chains(chains)
}

/// Order chains `default` first, then alphabetically - matching the store's list
/// order so the graph reads in the same order as the workspace list.
fn order_chains(mut chains: Vec<Chain>) -> Vec<Chain> {
    chains.sort_by(|a, b| {
        let key = |w: &str| (w != crate::store::DEFAULT_WORKSPACE, w.to_string());
        key(&a.workspace).cmp(&key(&b.workspace))
    });
    chains
}

/// First line of a description, trimmed; a placeholder for the empty case so a
/// working-copy commit never renders as a blank row.
fn summary(description: &str) -> String {
    let first = description.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        "(no description)".to_string()
    } else {
        first.to_string()
    }
}

/// jj shows change ids in a reverse-hex alphabet (`0-9a-f` -> `z-k`) so they are
/// visually distinct from commit ids. Reproduce it for the first `len` digits.
fn display_change_id(hex: &str, len: usize) -> String {
    const REV: &[u8; 16] = b"zyxwvutsrqponmlk";
    hex.chars()
        .take(len)
        .map(|c| c.to_digit(16).map(|d| REV[d as usize] as char).unwrap_or(c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_id_maps_hex_to_jj_reverse_alphabet() {
        // '0' -> 'z', 'f' -> 'k', and a mixed prefix.
        assert_eq!(display_change_id("0", 1), "z");
        assert_eq!(display_change_id("f", 1), "k");
        assert_eq!(display_change_id("0f", 2), "zk");
        assert_eq!(display_change_id("abcdef012345", 8), "ponmlkzy");
    }

    #[test]
    fn summary_placeholders_the_empty_description() {
        assert_eq!(summary(""), "(no description)");
        assert_eq!(summary("   \n"), "(no description)");
        assert_eq!(summary("first line\nsecond"), "first line");
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

    /// Build a minimal node map from `(id, parent, summary)` triples. `parent`
    /// empty means no parent (the root).
    fn nodes(rows: &[(&str, &str, &str)]) -> BTreeMap<String, Node> {
        rows.iter()
            .map(|(id, parent, summary)| {
                let node = Node {
                    id: id.to_string(),
                    change_id: id.to_string(),
                    summary: summary.to_string(),
                    parents: if parent.is_empty() {
                        vec![]
                    } else {
                        vec![parent.to_string()]
                    },
                    bookmarks: vec![],
                    timestamp_ms: 0,
                    wc_of: vec![],
                };
                (node.id.clone(), node)
            })
            .collect()
    }

    #[test]
    fn chain_walks_at_down_to_the_trunk_boundary() {
        // trunk = t1; a workspace branches t1 -> b1 -> b2(@).
        let nodes = nodes(&[
            ("t0", "", "root"),
            ("t1", "t0", "main"),
            ("b1", "t1", "wip"),
            ("b2", "b1", "@"),
        ]);
        let wc = BTreeMap::from([("feat".to_string(), "b2".to_string())]);
        let trunk_ancestors = HashSet::from(["t1".to_string(), "t0".to_string()]);

        let chains = build_chains(&nodes, &wc, &trunk_ancestors);
        let feat = &chains[0];
        assert_eq!(feat.commits, vec!["b2", "b1"]); // trunk-exclusive, head->base
        assert_eq!(feat.base.as_deref(), Some("t1")); // attaches to trunk tip
        assert_eq!(feat.child, None); // @ is a leaf
    }

    #[test]
    fn chain_never_pushed_trunk_is_root() {
        // No bookmarks: trunk resolved to the root commit. A workspace off root
        // must still yield a bounded chain (its own commits), not the whole world.
        let nodes = nodes(&[("r", "", "root"), ("c1", "r", "work"), ("c2", "c1", "@")]);
        let wc = BTreeMap::from([("default".to_string(), "c2".to_string())]);
        let trunk_ancestors = HashSet::from(["r".to_string()]);

        let chains = build_chains(&nodes, &wc, &trunk_ancestors);
        assert_eq!(chains[0].commits, vec!["c2", "c1"]);
        assert_eq!(chains[0].base.as_deref(), Some("r"));
    }

    #[test]
    fn chain_surfaces_one_child_past_at() {
        // @ is b1 but a child b2 exists above it (e.g. after `jj edit b1`).
        let nodes = nodes(&[
            ("t1", "", "main"),
            ("b1", "t1", "@ edited"),
            ("b2", "b1", "child"),
        ]);
        let wc = BTreeMap::from([("feat".to_string(), "b1".to_string())]);
        let trunk_ancestors = HashSet::from(["t1".to_string()]);

        let chains = build_chains(&nodes, &wc, &trunk_ancestors);
        assert_eq!(chains[0].commits, vec!["b1"]);
        assert_eq!(chains[0].child.as_deref(), Some("b2")); // the n+1
    }

    #[test]
    fn chain_clean_workspace_on_trunk_tip_is_empty() {
        // default @ sits on the trunk tip: no trunk-exclusive commits.
        let nodes = nodes(&[("t1", "", "main")]);
        let wc = BTreeMap::from([("default".to_string(), "t1".to_string())]);
        let trunk_ancestors = HashSet::from(["t1".to_string()]);

        let chains = build_chains(&nodes, &wc, &trunk_ancestors);
        assert!(chains[0].commits.is_empty());
        assert_eq!(chains[0].base.as_deref(), Some("t1"));
    }

    #[test]
    fn chains_order_default_first_then_alphabetical() {
        let nodes = nodes(&[("t1", "", "main")]);
        let wc = BTreeMap::from([
            ("zeta".to_string(), "t1".to_string()),
            ("default".to_string(), "t1".to_string()),
            ("alpha".to_string(), "t1".to_string()),
        ]);
        let trunk_ancestors = HashSet::from(["t1".to_string()]);

        let chains = build_chains(&nodes, &wc, &trunk_ancestors);
        let order: Vec<_> = chains.iter().map(|c| c.workspace.as_str()).collect();
        assert_eq!(order, ["default", "alpha", "zeta"]);
    }
}
