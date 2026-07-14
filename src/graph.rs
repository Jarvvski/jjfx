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
//! ([`build`]) that is unit-tested without a real store. Trunk selection lives in
//! [`crate::trunk`], shared with the CLI reads.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::{Context, anyhow};
use jj_lib::backend::CommitId;
use jj_lib::config::StackedConfig;
use jj_lib::object_id::ObjectId;
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
    /// Whether this commit is immutable in jj's default sense - an ancestor of
    /// `trunk()`, a tag, or an untracked remote bookmark (other people's work).
    pub immutable: bool,
}

/// One workspace's chain: its own commits above the immutable boundary, the
/// boundary commit it attaches to, and the one child past `@` (the "n+1").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chain {
    pub workspace: String,
    /// The working-copy `@` commit id (may equal `base` for a clean workspace
    /// sitting on the trunk tip, in which case `commits` is empty).
    pub head: String,
    /// The workspace's own mutable commits, ordered `@` (or nearest) down to
    /// the branch point. Empty when the workspace sits directly on trunk.
    pub commits: Vec<String>,
    /// The immutable commit (usually on trunk) this chain branches from, if it
    /// was reached.
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

    let trunk_id = crate::trunk::resolve(view, store, root.clone());

    // The immutable boundary heads, mirroring jj's `builtin_immutable_heads()`:
    // trunk, tags, and untracked remote bookmarks (other people's work). The
    // colocated `git` pseudo-remote is not a real remote and does not count
    // (same rule as `crate::trunk`).
    let mut immutable_heads: Vec<CommitId> = vec![trunk_id.clone()];
    for (symbol, remote_ref) in view.all_remote_bookmarks() {
        if symbol.remote != jj_lib::git::REMOTE_NAME_FOR_LOCAL_GIT_REPO
            && !remote_ref.is_tracked()
            && let Some(id) = remote_ref.target.as_normal()
        {
            immutable_heads.push(id.clone());
        }
    }
    for (_, target) in view.tags() {
        if let Some(id) = target.local_target.as_normal() {
            immutable_heads.push(id.clone());
        }
    }

    // Phase A: immutable ancestors (bounded), so we can tell where a branch
    // rejoins already-landed history without walking past it.
    let mut loaded: HashMap<CommitId, jj_lib::commit::Commit> = HashMap::new();
    let mut immutable: HashSet<CommitId> = HashSet::new();
    let mut queue: VecDeque<CommitId> = immutable_heads.into_iter().collect();
    while let Some(id) = queue.pop_front() {
        if immutable.contains(&id) || immutable.len() >= IMMUTABLE_ANCESTOR_CAP {
            continue;
        }
        let commit = match store.get_commit(&id) {
            Ok(c) => c,
            Err(_) => continue,
        };
        immutable.insert(id.clone());
        for p in commit.parent_ids() {
            queue.push_back(p.clone());
        }
        loaded.insert(id, commit);
    }

    // Phase B: the mutable neighbourhood. Seed with every head and every `@`
    // (heads capture children of `@`); walk parents but stop at the immutable
    // boundary so we never pull in deep landed history.
    let mut seeds: Vec<CommitId> = view.heads().iter().cloned().collect();
    seeds.extend(wc.values().cloned());
    let mut expanded: HashSet<CommitId> = HashSet::new();
    let mut queue: VecDeque<CommitId> = seeds.into_iter().collect();
    while let Some(id) = queue.pop_front() {
        if !expanded.insert(id.clone()) || expanded.len() > NEIGHBOURHOOD_CAP {
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
        // Boundary: an immutable commit is an anchor, not something to expand
        // through.
        if immutable.contains(&id) {
            continue;
        }
        for p in parents {
            queue.push_back(p);
        }
    }

    Ok(build(&loaded, &bookmarks, &wc, &immutable, &trunk_id))
}

/// Depth caps: bounded so a pathological repo never stalls the read. The
/// immutable walk must cover the whole landed history (a branch point below the
/// cap would misclassify everything under it as mutable), so its cap is sized
/// for very large repos, not typical ones.
const IMMUTABLE_ANCESTOR_CAP: usize = 50_000;
const NEIGHBOURHOOD_CAP: usize = 3000;
/// Longest single workspace chain we will render before giving up (graceful).
const CHAIN_CAP: usize = 500;

/// Assemble the graph from loaded commits. Projects the jj-lib `Commit` map into
/// plain string maps up front, then delegates the chain/boundary logic to the
/// pure [`build_chains`] so that logic is unit-testable without a real store.
fn build(
    loaded: &HashMap<CommitId, jj_lib::commit::Commit>,
    bookmarks: &HashMap<CommitId, Vec<String>>,
    wc: &BTreeMap<String, CommitId>,
    immutable: &HashSet<CommitId>,
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
                immutable: immutable.contains(id),
                id: hex.clone(),
            };
            (hex, node)
        })
        .collect();

    let immutable: HashSet<String> = immutable.iter().map(ObjectId::hex).collect();
    let wc_hex: BTreeMap<String, String> = wc.iter().map(|(n, id)| (n.clone(), id.hex())).collect();
    let chains = build_chains(&nodes, &wc_hex, &immutable);

    Graph {
        trunk_id: loaded.contains_key(trunk_id).then(|| trunk_id.hex()),
        chains,
        nodes,
    }
}

/// Pure per-workspace chain derivation over the string node map. Each chain is a
/// first-parent walk from `@` down to the immutable boundary, plus the one child
/// past `@`. First-parent keeps the chain linear; merges (rare in these
/// workspaces) show only their mainline side, which is acceptable for v1.
fn build_chains(
    nodes: &BTreeMap<String, Node>,
    wc: &BTreeMap<String, String>,
    immutable: &HashSet<String>,
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
            while !immutable.contains(&cur) && depth < CHAIN_CAP {
                commits.push(cur.clone());
                let Some(parent) = nodes.get(&cur).and_then(|n| n.parents.first()).cloned() else {
                    break;
                };
                cur = parent;
                depth += 1;
            }
            let base = immutable.contains(&cur).then(|| cur.clone());

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

/// An edge from a displayed log row to what lies beneath it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogEdge {
    /// The parent commit is itself displayed.
    Direct(String),
    /// A displayed ancestor reached through elided (undisplayed) commits.
    Elided(String),
    /// History continues below but reaches nothing displayed (renders `~`).
    Missing,
}

/// One row of the jj-log-style world view: a displayed commit and its edges,
/// in render order (children always before parents).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRow {
    pub id: String,
    pub edges: Vec<LogEdge>,
}

/// Derive the world view's rows: every mutable commit, every workspace's `@`,
/// the full connected trunk history (`::trunk()`), and each fragment's
/// immutable branch point - with only off-trunk history elided.
pub fn log_rows(g: &Graph) -> Vec<LogRow> {
    let mut shown: HashSet<&str> = g
        .nodes
        .values()
        .filter(|n| !n.immutable || !n.wc_of.is_empty())
        .map(|n| n.id.as_str())
        .collect();
    // The trunk spine: every loaded ancestor of the trunk tip, shown connected
    // (the `::trunk()` mainline history a jj log is read against).
    if let Some(tid) = &g.trunk_id {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut queue: VecDeque<&str> = VecDeque::from([tid.as_str()]);
        while let Some(id) = queue.pop_front() {
            let Some(node) = g.nodes.get(id) else {
                continue;
            };
            if !seen.insert(&node.id) {
                continue;
            }
            shown.insert(&node.id);
            queue.extend(node.parents.iter().map(String::as_str));
        }
    }
    // Branch points: the immutable parent each mutable fragment sits on
    // (already covered when it is on the trunk spine).
    let context: Vec<&str> = g
        .nodes
        .values()
        .filter(|n| !n.immutable && shown.contains(n.id.as_str()))
        .flat_map(|n| &n.parents)
        .filter_map(|p| g.nodes.get(p))
        .filter(|p| p.immutable)
        .map(|p| p.id.as_str())
        .collect();
    shown.extend(context);

    // Classify each shown commit's parent edges; elided gaps (undisplayed
    // ancestors between two shown commits, e.g. along the trunk) walk down
    // until they resurface at a shown commit.
    let edges: HashMap<&str, Vec<LogEdge>> = shown
        .iter()
        .map(|&id| {
            let node = &g.nodes[id];
            let mut out = Vec::new();
            for p in &node.parents {
                if shown.contains(p.as_str()) {
                    out.push(LogEdge::Direct(p.clone()));
                } else {
                    out.extend(walk_to_shown(g, &shown, p));
                }
            }
            // Two parents can elide to the same ancestor (or both fall off the
            // loaded set); collapse the duplicates so they take one column.
            let mut uniq = Vec::new();
            for e in out {
                if !uniq.contains(&e) {
                    uniq.push(e);
                }
            }
            (id, uniq)
        })
        .collect();

    order_rows(g, &shown, &edges)
}

/// From an undisplayed commit, walk ancestors until each path resurfaces at a
/// shown commit ([`LogEdge::Elided`]) or leaves the loaded set
/// ([`LogEdge::Missing`]). Deduplicated; bounded by the loaded set.
fn walk_to_shown(g: &Graph, shown: &HashSet<&str>, from: &str) -> Vec<LogEdge> {
    let mut out = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut queue: VecDeque<&str> = VecDeque::from([from]);
    let mut missing = false;
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        if shown.contains(id) {
            if !out.contains(&LogEdge::Elided(id.to_string())) {
                out.push(LogEdge::Elided(id.to_string()));
            }
            continue;
        }
        match g.nodes.get(id) {
            Some(n) if !n.parents.is_empty() => queue.extend(n.parents.iter().map(String::as_str)),
            // The root commit, or a parent beyond the load caps: history ends
            // or continues out of view - either way a `~` below.
            _ => missing = true,
        }
    }
    if missing || out.is_empty() {
        out.push(LogEdge::Missing);
    }
    out
}

/// The displayed ancestor a [`LogEdge`] points at, if any.
fn edge_target(e: &LogEdge) -> Option<&str> {
    match e {
        LogEdge::Direct(p) | LogEdge::Elided(p) => Some(p),
        LogEdge::Missing => None,
    }
}

/// Topologically order the shown commits, children before parents, the way a
/// jj log reads: the working-copy chains lead (`default` first, mirroring
/// jj's `log-graph-prioritize`), each fragment stays contiguous and sits
/// beside its branch point, and the trunk spine flows on down between them.
fn order_rows(
    g: &Graph,
    shown: &HashSet<&str>,
    edges: &HashMap<&str, Vec<LogEdge>>,
) -> Vec<LogRow> {
    let mut indeg: HashMap<&str, usize> = shown.iter().map(|&id| (id, 0)).collect();
    for out in edges.values() {
        for e in out.iter().filter_map(edge_target) {
            if let Some(d) = indeg.get_mut(e) {
                *d += 1;
            }
        }
    }

    // A commit's pick priority: its workspace chain's position (`default`
    // first, then alphabetical - the chains' order), then recency.
    let mut chain_rank: HashMap<&str, usize> = HashMap::new();
    for (i, chain) in g.chains.iter().enumerate() {
        chain_rank.entry(chain.head.as_str()).or_insert(i);
        for c in &chain.commits {
            chain_rank.entry(c.as_str()).or_insert(i);
        }
    }
    let key = |id: &str| {
        (
            chain_rank.get(id).copied().unwrap_or(usize::MAX),
            std::cmp::Reverse(g.nodes[id].timestamp_ms),
            id.to_string(),
        )
    };

    let mut ready: Vec<&str> = indeg
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(id, _)| *id)
        .collect();
    let mut rows = Vec::with_capacity(shown.len());
    // The parent we are working towards - it keeps a chain contiguous, and
    // while it is blocked by other children (sibling fragments), those emit
    // first so every fragment lands beside its branch point.
    let mut goal: Option<String> = None;
    while !ready.is_empty() {
        let pos = goal
            .as_ref()
            .and_then(|goa| {
                ready.iter().position(|id| id == goa).or_else(|| {
                    // The goal still waits on other children; emit one of them.
                    ready
                        .iter()
                        .enumerate()
                        .filter(|(_, id)| {
                            edges[**id].iter().filter_map(edge_target).any(|t| t == goa)
                        })
                        .min_by_key(|(_, id)| key(id))
                        .map(|(i, _)| i)
                })
            })
            .unwrap_or_else(|| {
                ready
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, id)| key(id))
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            });
        let id = ready.swap_remove(pos);
        let out = edges.get(id).cloned().unwrap_or_default();
        for e in out.iter().filter_map(edge_target) {
            if let Some(d) = indeg.get_mut(e) {
                *d -= 1;
                if *d == 0 {
                    ready.push(&g.nodes[e].id);
                }
            }
        }
        // Advance the goal: reaching it (or starting fresh) chases this
        // commit's first parent next; a sibling emitted while the goal still
        // waits keeps the goal in place.
        let sibling_of_goal = goal
            .as_ref()
            .is_some_and(|goa| out.iter().filter_map(edge_target).any(|t| t == goa));
        if goal.as_deref() == Some(id) || !sibling_of_goal {
            goal = out.iter().find_map(edge_target).map(str::to_string);
        }
        rows.push(LogRow {
            id: id.to_string(),
            edges: out,
        });
    }
    rows
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
                    immutable: false,
                };
                (node.id.clone(), node)
            })
            .collect()
    }

    /// A Graph for `log_rows` tests: `(id, parents, immutable, wc_of)` rows,
    /// timestamps descending in declaration order (first row newest).
    fn graph_of(rows: &[(&str, &[&str], bool, &[&str])], trunk_id: &str) -> Graph {
        let count = rows.len() as i64;
        let nodes: BTreeMap<String, Node> = rows
            .iter()
            .enumerate()
            .map(|(i, (id, parents, immutable, wc_of))| {
                let node = Node {
                    id: id.to_string(),
                    change_id: id.to_string(),
                    summary: format!("summary {id}"),
                    parents: parents.iter().map(|p| p.to_string()).collect(),
                    bookmarks: vec![],
                    timestamp_ms: count - i as i64,
                    wc_of: wc_of.iter().map(|w| w.to_string()).collect(),
                    immutable: *immutable,
                };
                (node.id.clone(), node)
            })
            .collect();
        Graph {
            nodes,
            trunk_id: Some(trunk_id.to_string()),
            chains: vec![],
        }
    }

    fn row_ids(rows: &[LogRow]) -> Vec<&str> {
        rows.iter().map(|r| r.id.as_str()).collect()
    }

    #[test]
    fn log_rows_orders_children_before_parents_and_keeps_chains_contiguous() {
        // Two workspaces branch off the trunk tip; a chain's commits must stay
        // together, newest chain first, and the trunk spine flows connected
        // below everything that branches from it.
        let g = graph_of(
            &[
                ("b2", &["b1"], false, &["feat"]),
                ("b1", &["t1"], false, &[]),
                ("a1", &["t1"], false, &["default"]),
                ("t1", &["t0"], true, &[]),
                ("t0", &[], true, &[]),
            ],
            "t1",
        );
        let rows = log_rows(&g);
        assert_eq!(row_ids(&rows), ["b2", "b1", "a1", "t1", "t0"]);
        // The whole trunk history is shown (`::trunk()`), so the tip connects
        // straight down the spine rather than eliding it away.
        assert_eq!(rows[3].edges, vec![LogEdge::Direct("t0".to_string())]);
    }

    #[test]
    fn log_rows_shows_the_whole_trunk_spine_connected() {
        // A workspace on an older trunk commit: every trunk commit between the
        // tip and its branch point renders, connected by direct edges.
        let g = graph_of(
            &[
                ("a1", &["t0"], false, &["old"]),
                ("t1", &["t_mid"], true, &[]),
                ("t_mid", &["t0"], true, &[]),
                ("t0", &[], true, &[]),
            ],
            "t1",
        );
        let rows = log_rows(&g);
        assert_eq!(row_ids(&rows), ["a1", "t1", "t_mid", "t0"]);
        let t1 = rows.iter().find(|r| r.id == "t1").unwrap();
        assert_eq!(t1.edges, vec![LogEdge::Direct("t_mid".to_string())]);
    }

    #[test]
    fn log_rows_elides_offtrunk_immutable_history_to_the_spine() {
        // A fragment based on someone else's (immutable, off-trunk) branch:
        // its branch point shows as an anchor whose hidden history walks down
        // until it rejoins the trunk spine through an Elided edge.
        let g = graph_of(
            &[
                ("m", &["anchor"], false, &[]),
                ("anchor", &["gap"], true, &[]),
                ("gap", &["t0"], true, &[]),
                ("t1", &["t0"], true, &[]),
                ("t0", &[], true, &[]),
            ],
            "t1",
        );
        let rows = log_rows(&g);
        assert!(!row_ids(&rows).contains(&"gap"), "gap commits stay hidden");
        let anchor = rows.iter().find(|r| r.id == "anchor").unwrap();
        assert_eq!(anchor.edges, vec![LogEdge::Elided("t0".to_string())]);
    }

    #[test]
    fn log_rows_shows_a_clean_workspace_sitting_on_the_trunk() {
        // `@` on a trunk commit keeps its workspace badge on the spine row.
        let g = graph_of(
            &[
                ("t1", &["t_mid"], true, &[]),
                ("t_mid", &["t0"], true, &["behind"]),
                ("t0", &[], true, &[]),
            ],
            "t1",
        );
        let rows = log_rows(&g);
        assert_eq!(row_ids(&rows), ["t1", "t_mid", "t0"]);
    }

    #[test]
    fn log_rows_puts_the_default_workspace_chain_first() {
        // feat's head is newer, but the default workspace's chain leads
        // (mirroring jj's `log-graph-prioritize` on `@`).
        let mut g = graph_of(
            &[
                ("f1", &["t1"], false, &["feat"]),
                ("d1", &["t1"], false, &["default"]),
                ("t1", &[], true, &[]),
            ],
            "t1",
        );
        g.chains = vec![
            Chain {
                workspace: "default".to_string(),
                head: "d1".to_string(),
                commits: vec!["d1".to_string()],
                base: Some("t1".to_string()),
                child: None,
            },
            Chain {
                workspace: "feat".to_string(),
                head: "f1".to_string(),
                commits: vec!["f1".to_string()],
                base: Some("t1".to_string()),
                child: None,
            },
        ];
        let rows = log_rows(&g);
        assert_eq!(row_ids(&rows), ["d1", "f1", "t1"]);
    }

    #[test]
    fn log_rows_merge_keeps_both_parent_edges() {
        let g = graph_of(
            &[
                ("m", &["a1", "b1"], false, &["feat"]),
                ("a1", &["t1"], false, &[]),
                ("b1", &["t1"], false, &[]),
                ("t1", &[], true, &[]),
            ],
            "t1",
        );
        let rows = log_rows(&g);
        assert_eq!(
            rows[0].edges,
            vec![
                LogEdge::Direct("a1".to_string()),
                LogEdge::Direct("b1".to_string()),
            ]
        );
        assert_eq!(row_ids(&rows).last(), Some(&"t1"));
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
