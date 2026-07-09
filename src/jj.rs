//! Minimal jj reads via the CLI. The skeleton only needs the set of workspace
//! *names* jj knows about; jj does not record each workspace's filesystem path
//! (spike 02), so paths come from the ws-cache. Richer reads move to jj-lib in
//! later epics (ADR 0007).

use std::path::Path;

use crate::cmd::{Run, cmd};

/// Workspace names jj knows in this repo, via
/// `jj workspace list -T 'name ++ "\n"'`. `--ignore-working-copy` keeps it a
/// pure read (no snapshot, no commit churn). Returns an empty list on any
/// failure - the skeleton degrades to the ws-cache + derived default rather than
/// erroring, and the caller cannot surface errors from inside the alt-screen.
pub fn workspace_names(repo_root: &Path) -> Vec<String> {
    let out = cmd("jj")
        .arg("--repository")
        .arg(repo_root)
        .arg("--ignore-working-copy")
        .args(["workspace", "list", "-T", "name ++ \"\\n\""])
        .run()
        .ok()
        .and_then(Run::stdout_ok);

    out.into_iter()
        .flat_map(|s| {
            s.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Create a new named workspace rooted at `dest` (`jj workspace add`). Unlike the
/// read paths, this is a mutation whose failure the caller surfaces, so it
/// returns a `Result` with the jj error text on failure.
pub fn add_workspace(repo_root: &Path, name: &str, dest: &Path) -> anyhow::Result<()> {
    run_mut(
        repo_root,
        &["workspace", "add", "--name", name, &dest.to_string_lossy()],
    )
}

/// Forget a workspace (`jj workspace forget`), removing jj's record of it. The
/// on-disk directory is removed separately by the caller.
pub fn forget_workspace(repo_root: &Path, name: &str) -> anyhow::Result<()> {
    run_mut(repo_root, &["workspace", "forget", name])
}

/// Source revset for `tidyws`: idle, empty, undescribed workspace working-copies
/// (excluding any that carry a bookmark or tag). Ported from the `tidyws` alias.
const TIDYWS_SRC: &str =
    "working_copies() & empty() & description(exact:'') ~ bookmarks() ~ tags()";

/// Revset for `tidy`: junk mutable empties - undescribed, unbookmarked, untagged,
/// and never the current `@`. Ported from the `tidy` alias.
const TIDY_REVSET: &str = "mutable() & empty() & description(exact:'') ~ @ ~ bookmarks() ~ tags()";

/// Reset idle, empty, undescribed workspace working-copies onto the trunk base
/// (`jj rebase -s <ws-empties> -d TRUNK_BASE`), returning how many matched. A
/// no-op (returning 0) when nothing is eligible, so it never errors on an empty
/// repo. Rebases onto [`crate::work::TRUNK_BASE`] - the same base the `behind`
/// indicator measures against - rather than jj's raw `trunk()`, so tidying an idle
/// empty workspace actually zeroes its `behind` count even when local `main` is
/// ahead of `origin/main`. `--ignore-immutable` mirrors the alias; workspaces with
/// real work are excluded by the `empty()`/`description(exact:'')` filters.
pub fn tidyws(repo_root: &Path) -> anyhow::Result<usize> {
    let n = count(repo_root, TIDYWS_SRC);
    if n == 0 {
        return Ok(0);
    }
    run_mut(
        repo_root,
        &[
            "rebase",
            "--ignore-immutable",
            "-s",
            TIDYWS_SRC,
            "-d",
            crate::work::TRUNK_BASE,
        ],
    )?;
    Ok(n)
}

/// Source revset for lifting one workspace's own stack: the root(s) of its mutable
/// chain reachable from `<ws>@`. `-s` on this moves the whole stack, so it adapts
/// to shape on its own - a lone empty `@`, or a multi-commit stack - without
/// choosing between `-r`/`-s`.
fn lift_src(ws: &str) -> String {
    format!("roots(mutable() & mine() & ::{ws}@)")
}

/// Rebase one workspace's own mutable stack onto the trunk base
/// ([`crate::work::TRUNK_BASE`]), lifting it up to date **without pushing** - the
/// local remedy for a `behind` workspace, for empty and non-empty alike. `-s`
/// moves the whole stack; `--skip-emptied` drops commits the rebase makes empty
/// (e.g. an idle empty `@`, recreated on trunk). Returns `false` when the
/// workspace has nothing of ours to lift. Idempotent: a no-op when already on the
/// trunk tip.
pub fn lift(repo_root: &Path, ws: &str) -> anyhow::Result<bool> {
    let src = lift_src(ws);
    if count(repo_root, &src) == 0 {
        return Ok(false);
    }
    run_mut(
        repo_root,
        &[
            "rebase",
            "--skip-emptied",
            "-s",
            &src,
            "-d",
            crate::work::TRUNK_BASE,
        ],
    )?;
    Ok(true)
}

/// Lift every workspace's stack onto the trunk base in one rebase (the bulk form
/// of [`lift`], keyed off every workspace's `@` via `working_copies()`). Returns
/// `false` when there is nothing to lift.
pub fn lift_all(repo_root: &Path) -> anyhow::Result<bool> {
    let src = "roots(mutable() & mine() & ::working_copies())";
    if count(repo_root, src) == 0 {
        return Ok(false);
    }
    run_mut(
        repo_root,
        &[
            "rebase",
            "--skip-emptied",
            "-s",
            src,
            "-d",
            crate::work::TRUNK_BASE,
        ],
    )?;
    Ok(true)
}

/// Abandon junk empties (`jj abandon <tidy-revset>`), returning how many matched.
/// A no-op (returning 0) when nothing is eligible. Destructive - callers confirm
/// first.
pub fn tidy(repo_root: &Path) -> anyhow::Result<usize> {
    let n = count(repo_root, TIDY_REVSET);
    if n == 0 {
        return Ok(0);
    }
    run_mut(repo_root, &["abandon", TIDY_REVSET])?;
    Ok(n)
}

/// Count the revisions matching `revset` via a pure read (`--ignore-working-copy`,
/// no snapshot). Zero on any failure - the caller then treats it as "nothing to
/// do" rather than erroring.
fn count(repo_root: &Path, revset: &str) -> usize {
    cmd("jj")
        .arg("--repository")
        .arg(repo_root)
        .arg("--ignore-working-copy")
        .args(["log", "-r", revset, "--no-graph", "-T", "\"x\\n\""])
        .run()
        .ok()
        .and_then(Run::stdout_ok)
        .map(|s| s.lines().filter(|l| !l.is_empty()).count())
        .unwrap_or(0)
}

/// Run a mutating jj command, returning an error carrying jj's stderr on failure.
fn run_mut(repo_root: &Path, args: &[&str]) -> anyhow::Result<()> {
    cmd("jj")
        .arg("--repository")
        .arg(repo_root)
        .args(args)
        .run()?
        .checked()?;
    Ok(())
}
