//! Minimal jj reads via the CLI. The skeleton only needs the set of workspace
//! *names* jj knows about; jj does not record each workspace's filesystem path
//! (spike 02), so paths come from the ws-cache. Richer reads move to jj-lib in
//! later epics (ADR 0007).

use std::path::{Path, PathBuf};

use crate::cmd::cmd;

/// Run a read-only jj command against the repo, returning stdout on success. The
/// single home for the read incantation: `--repository <repo_root>
/// --ignore-working-copy` keeps it a pure read - jjfx must never snapshot the
/// working copy (that would churn commits and ping its own watcher, ADR 0006).
/// Callers project the `Result` to whatever they need (`.ok()` to degrade
/// silently, `map_err` for a string error).
pub fn read_at_repo(repo_root: &Path, args: &[&str]) -> anyhow::Result<String> {
    cmd("jj")
        .arg("--repository")
        .arg(repo_root)
        .arg("--ignore-working-copy")
        .args(args)
        .run()?
        .checked()
}

/// Run a read-only jj command in `dir`, so `@` resolves to that workspace's own
/// working copy. Like [`read_at_repo`], `--ignore-working-copy` keeps it a pure
/// read.
pub fn read_in_dir(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    cmd("jj")
        .current_dir(dir)
        .arg("--ignore-working-copy")
        .args(args)
        .run()?
        .checked()
}

/// Derive the `owner/repo` slug from jj's `origin` remote URL. Every `gh` call
/// must pass this explicitly because auto-detection fails in jj workspaces.
pub(crate) fn derive_repo_slug(repo_root: &Path) -> anyhow::Result<Option<String>> {
    let out = read_at_repo(repo_root, &["git", "remote", "list"])?;
    let Some(url) = out
        .lines()
        .filter_map(|line| line.split_once(char::is_whitespace))
        .find(|(name, _)| *name == "origin")
        .map(|(_, url)| url.trim())
    else {
        return Ok(None);
    };
    Ok(slug_from_url(url))
}

fn slug_from_url(url: &str) -> Option<String> {
    let url = url.strip_suffix(".git").unwrap_or(url);
    let parts: Vec<&str> = url
        .split(['/', ':'])
        .filter(|part| !part.is_empty())
        .collect();
    if parts.len() < 2 {
        return None;
    }
    Some(format!(
        "{}/{}",
        parts[parts.len() - 2],
        parts[parts.len() - 1]
    ))
}

/// Workspace names jj knows in this repo, via
/// `jj workspace list -T 'stringify(name) ++ "\n"'`. `stringify` is required
/// because `name` is a symbol: jj renders a name that needs quoting (`my work`)
/// with its quotes, and a caller wants the raw name jj and the ws-cache agree on.
/// `--ignore-working-copy` keeps it a pure read (no snapshot, no commit churn).
/// Returns an empty list on any failure - the skeleton degrades to the ws-cache +
/// derived default rather than erroring, and the caller cannot surface errors
/// from inside the alt-screen.
pub fn workspace_names(repo_root: &Path) -> Vec<String> {
    let out = read_at_repo(
        repo_root,
        &["workspace", "list", "-T", "stringify(name) ++ \"\\n\""],
    )
    .ok();

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

/// Create a new named workspace rooted at `dest` (`jj workspace add`). Workspace
/// lifecycle ordering lives in [`crate::store::Store`]; this function is only
/// the local jj adapter for its critical mutation step.
#[cfg(test)]
pub(crate) fn add_workspace(repo_root: &Path, name: &str, dest: &Path) -> anyhow::Result<()> {
    run_mut(
        repo_root,
        &[
            "workspace",
            "add",
            "--name",
            name,
            "--no-colocate",
            &dest.to_string_lossy(),
        ],
    )
}

/// Source revset for `tidyws`: idle, empty, undescribed workspace working-copies
/// (excluding any that carry a bookmark or tag). Ported from the `tidyws` alias.
const TIDYWS_SRC: &str =
    "working_copies() & empty() & description(exact:'') ~ bookmarks() ~ tags()";

/// Revset for `tidy`: junk mutable empties - undescribed, unbookmarked, untagged,
/// and never the current `@`. Ported from the `tidy` alias.
const TIDY_REVSET: &str = "mutable() & empty() & description(exact:'') ~ @ ~ bookmarks() ~ tags()";

/// A workspace or ref name as a jj revset symbol: double-quoted and escaped so a
/// name containing spaces or revset metacharacters cannot be read as revset
/// syntax. jj accepts a quoted string wherever it accepts a symbol, so
/// `"my work"@` names the same working copy `my work@` would if it parsed at all.
/// Only backslash and double quote need escaping - jj permits both inside a
/// workspace name, and every other character is literal in a string.
pub(crate) fn revset_symbol(name: &str) -> String {
    let mut symbol = String::with_capacity(name.len() + 2);
    symbol.push('"');
    for character in name.chars() {
        if matches!(character, '"' | '\\') {
            symbol.push('\\');
        }
        symbol.push(character);
    }
    symbol.push('"');
    symbol
}

/// Source revset for lifting one workspace's own stack: the root(s) of its mutable
/// chain reachable from `<ws>@`. `-s` on this moves the whole stack, so it adapts
/// to shape on its own - a lone empty `@`, or a multi-commit stack - without
/// choosing between `-r`/`-s`.
fn lift_src(ws: &str) -> String {
    format!("roots(mutable() & mine() & ::{}@)", revset_symbol(ws))
}

/// Repository-maintenance mutations behind the App's existing test seam. Unlike
/// the read paths, each surfaces jj's error text for footer presentation.
pub trait Jj: Send + Sync {
    /// Reset idle, empty, undescribed workspace working-copies onto the trunk base
    /// (`jj rebase -s <ws-empties> -d <trunk>`), returning how many matched. A
    /// no-op (returning 0) when nothing is eligible, so it never errors on an empty
    /// repo. Rebases onto [`crate::trunk::as_revset`] - the same base the `behind`
    /// indicator measures against - rather than jj's raw `trunk()`, so tidying an
    /// idle empty workspace actually zeroes its `behind` count even when local
    /// `main` is ahead of `origin/main`. `--ignore-immutable` mirrors the alias;
    /// workspaces with real work are excluded by the
    /// `empty()`/`description(exact:'')` filters.
    fn tidyws(&self) -> anyhow::Result<usize>;

    /// Abandon junk empties (`jj abandon <tidy-revset>`), returning how many
    /// matched. A no-op (returning 0) when nothing is eligible. Destructive -
    /// callers confirm first.
    fn tidy(&self) -> anyhow::Result<usize>;
}

/// The real jj, driving the CLI against a fixed repo root. Each method performs
/// the mutation the equivalent bash alias did.
pub struct RealJj {
    repo_root: PathBuf,
}

impl RealJj {
    /// Bind the real jj to a repo root; every method runs `jj --repository <root>`.
    pub fn new(repo_root: PathBuf) -> Self {
        Self { repo_root }
    }
}

impl Jj for RealJj {
    fn tidyws(&self) -> anyhow::Result<usize> {
        let n = count(&self.repo_root, TIDYWS_SRC);
        if n == 0 {
            return Ok(0);
        }
        let trunk = crate::trunk::as_revset();
        run_mut(
            &self.repo_root,
            &[
                "rebase",
                "--ignore-immutable",
                "-s",
                TIDYWS_SRC,
                "-d",
                &trunk,
            ],
        )?;
        Ok(n)
    }

    fn tidy(&self) -> anyhow::Result<usize> {
        let n = count(&self.repo_root, TIDY_REVSET);
        if n == 0 {
            return Ok(0);
        }
        run_mut(&self.repo_root, &["abandon", TIDY_REVSET])?;
        Ok(n)
    }
}

/// Rebase one workspace's own mutable stack onto the trunk base
/// ([`crate::trunk::as_revset`]), lifting it up to date **without pushing** - the
/// local remedy for a `behind` workspace, for empty and non-empty alike. `-s`
/// moves the whole stack; `--skip-emptied` drops commits the rebase makes empty
/// (e.g. an idle empty `@`, recreated on trunk). Returns `false` when the
/// workspace has nothing of ours to lift. Idempotent: a no-op when already on the
/// trunk tip. Blocking - callers must run it off the render loop
/// (`spawn_blocking`), which is why it is a free function rather than a [`Jj`]
/// method (the boxed trait object cannot move to a worker thread).
pub fn lift(repo_root: &Path, ws: &str) -> anyhow::Result<bool> {
    let src = lift_src(ws);
    if count(repo_root, &src) == 0 {
        return Ok(false);
    }
    let trunk = crate::trunk::as_revset();
    run_mut(
        repo_root,
        &["rebase", "--skip-emptied", "-s", &src, "-d", &trunk],
    )?;
    Ok(true)
}

/// Lift every workspace's stack onto the trunk base in one rebase (the bulk form
/// of [`lift`], keyed off every workspace's `@` via `working_copies()`). Returns
/// `false` when there is nothing to lift. Blocking, like [`lift`].
pub fn lift_all(repo_root: &Path) -> anyhow::Result<bool> {
    let src = "roots(mutable() & mine() & ::working_copies())";
    if count(repo_root, src) == 0 {
        return Ok(false);
    }
    let trunk = crate::trunk::as_revset();
    run_mut(
        repo_root,
        &["rebase", "--skip-emptied", "-s", src, "-d", &trunk],
    )?;
    Ok(true)
}

/// Fetch from the git remote (`jj git fetch`), surfacing jj's error text on
/// failure. This is what advances `trunk()` and propagates remote branch
/// deletions (clearing a stale "merged" row) without running a full forge.
/// Blocking and network-bound - callers must run it off the render loop
/// (`spawn_blocking`), which is why it is a free function rather than a
/// [`Jj`] method (the boxed trait object cannot move to a worker thread).
pub fn fetch(repo_root: &Path) -> anyhow::Result<()> {
    run_mut(repo_root, &["git", "fetch"])
}

/// Count the revisions matching `revset` via a pure read (`--ignore-working-copy`,
/// no snapshot). Zero on any failure - the caller then treats it as "nothing to
/// do" rather than erroring.
fn count(repo_root: &Path, revset: &str) -> usize {
    read_at_repo(
        repo_root,
        &["log", "-r", revset, "--no-graph", "-T", "\"x\\n\""],
    )
    .ok()
    .map(|s| s.lines().filter(|l| !l.is_empty()).count())
    .unwrap_or(0)
}

/// Run a mutating jj command, returning an error carrying jj's stderr on failure.
fn run_mut(repo_root: &Path, args: &[&str]) -> anyhow::Result<()> {
    let command = cmd("jj");
    #[cfg(test)]
    let command = command.args(["--config", "signing.behavior=drop"]);
    command
        .arg("--repository")
        .arg(repo_root)
        .args(args)
        .run()?
        .checked()?;
    Ok(())
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
        assert_eq!(slug_from_url("nonsense"), None);
    }

    #[test]
    fn revset_symbol_quotes_names_that_are_not_bare_symbols() {
        // A bare identifier still round-trips quoted, and a name with a space or
        // revset metacharacter can no longer break out of the revset.
        assert_eq!(revset_symbol("feat"), "\"feat\"");
        assert_eq!(revset_symbol("my work"), "\"my work\"");
        assert_eq!(revset_symbol("a(b)"), "\"a(b)\"");
        // jj permits the quote characters themselves inside a workspace name.
        assert_eq!(revset_symbol("a\"b"), "\"a\\\"b\"");
        assert_eq!(revset_symbol("a\\b"), "\"a\\\\b\"");
    }

    #[test]
    fn lift_src_quotes_the_workspace_name() {
        assert_eq!(
            lift_src("my work"),
            "roots(mutable() & mine() & ::\"my work\"@)"
        );
    }

    /// A throwaway non-colocated jj repository for the real-jj read tests.
    fn temp_repo(tag: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the epoch")
            .as_nanos();
        let repo =
            std::env::temp_dir().join(format!("jjfx-jj-{tag}-{}-{nonce}", std::process::id()));
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

    #[test]
    fn workspace_names_returns_the_raw_name_not_the_quoted_symbol() {
        // `name` in `workspace list` is a symbol, so jj renders a name needing
        // quoting (`my work`) with quotes. The caller wants the raw name jj and
        // the ws-cache agree on.
        let repo = temp_repo("names");
        let root = repo.canonicalize().expect("temporary repo canonicalizes");
        let workspace = root
            .parent()
            .expect("temporary repository should have a parent")
            .join(format!(
                "{}-ws",
                root.file_name()
                    .expect("temporary repository should have a name")
                    .to_string_lossy()
            ));
        let status = std::process::Command::new("jj")
            .args([
                "--config",
                "signing.behavior=drop",
                "workspace",
                "add",
                "--name",
                "my work",
                "--no-colocate",
            ])
            .arg(&workspace)
            .current_dir(&root)
            .status()
            .expect("jj workspace add should run");
        assert!(status.success(), "jj workspace add should succeed");

        let names = workspace_names(&root);
        assert!(
            names.iter().any(|name| name == "my work"),
            "workspace_names must return the raw name, got {names:?}"
        );

        std::fs::remove_dir_all(&workspace).ok();
        std::fs::remove_dir_all(&repo).ok();
    }
}
