//! The one jj access layer: every jj semantic jjfx reads is derived here, from a
//! machine-readable source, and returned as a typed outcome.
//!
//! jj records a path for every named workspace, so jjfx reads both the name and
//! the path from jj rather than guessing one. Where a semantic needs jj state,
//! this layer reads it through jj's template language (`-T`), a purpose-built
//! flag (`--count`), or the Git diff format - never by scanning jj's English
//! status prose. A jj failure is a typed error ([`anyhow::Result`]), never a
//! silent empty/zero result: callers can always tell "jj said zero/none" from
//! "jj could not be read". Richer reads move to jj-lib in later epics (ADR 0007).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;

use crate::cmd::{Run, cmd};

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

/// A Workspace jj knows in this repo: its name and, when jj records one, its
/// directory.
///
/// jj keeps a path per named Workspace (`.jj/repo/workspace_store`), and
/// `jj workspace root --name <name>` reports it, so jjfx never has to guess one.
/// The `default` Workspace is the exception: it is implicit, jj records no path
/// for it, and its directory is the repository root (ADR 0008).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JjWorkspace {
    /// The raw Workspace name jj and the ws-cache agree on.
    pub name: String,
    /// The recorded directory, when jj has one for this Workspace.
    pub path: Option<PathBuf>,
}

/// Workspaces jj knows in this repo, via
/// `jj workspace list -T 'stringify(name) ++ "\t" ++ root ++ "\n"'`.
///
/// `stringify(name)` is required because `name` is a symbol: jj renders a name
/// that needs quoting (`my work`) with its quotes, and a caller wants the raw
/// name jj and the ws-cache agree on. `root` is empty for the implicit `default`
/// Workspace and a path for every other one. `--ignore-working-copy` keeps it a
/// pure read (no snapshot, no commit churn). A jj failure is a typed error,
/// never an empty list: the caller must be able to tell "jj reports no
/// workspaces" from "jj could not be read".
pub fn workspaces(repo_root: &Path) -> anyhow::Result<Vec<JjWorkspace>> {
    let out = read_at_repo(
        repo_root,
        &[
            "workspace",
            "list",
            "-T",
            "stringify(name) ++ \"\\t\" ++ root ++ \"\\n\"",
        ],
    )?;
    Ok(parse_workspaces(&out))
}

/// Workspace names jj knows in this repo.
pub fn workspace_names(repo_root: &Path) -> anyhow::Result<Vec<String>> {
    Ok(workspaces(repo_root)?
        .into_iter()
        .map(|workspace| workspace.name)
        .collect())
}

/// Split `jj workspace list` template output into the Workspaces it emitted.
///
/// One line per Workspace, `name<TAB>path`, where the path is empty when jj
/// records none. Only the first tab separates the two, so a path containing one
/// is kept whole. Blank lines are dropped.
fn parse_workspaces(out: &str) -> Vec<JjWorkspace> {
    out.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| match line.split_once('\t') {
            Some((name, path)) if !path.is_empty() => JjWorkspace {
                name: name.to_owned(),
                path: Some(PathBuf::from(path)),
            },
            Some((name, _)) => JjWorkspace {
                name: name.to_owned(),
                path: None,
            },
            None => JjWorkspace {
                name: line.to_owned(),
                path: None,
            },
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
        let n = count(&self.repo_root, TIDYWS_SRC)?;
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
        let n = count(&self.repo_root, TIDY_REVSET)?;
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
    if count(repo_root, &src)? == 0 {
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
    if count(repo_root, src)? == 0 {
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

/// Count the revisions matching `revset`, via jj's own `--count` - a structured
/// number, not a scraped line count. A jj failure is a typed error: a broken
/// revset must never read as "nothing to do".
fn count(repo_root: &Path, revset: &str) -> anyhow::Result<usize> {
    let out = read_at_repo(repo_root, &["log", "-r", revset, "--count"])?;
    parse_count(&out)
}

/// Parse the bare integer `jj log --count` prints. Any non-integer output is an
/// error, not a zero: the count's *absence* must be distinguishable.
fn parse_count(out: &str) -> anyhow::Result<usize> {
    out.trim()
        .parse()
        .with_context(|| format!("parsing `jj log --count` output {out:?}"))
}

/// The outcome of one `jj git push`, classified from jj's remote-bookmark state
/// rather than its English status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcome {
    /// A remote bookmark moved, so the remote was updated.
    Pushed,
    /// jj exited zero and left every remote bookmark where it was.
    Nothing,
    /// The push failed; carries jj's own message.
    Failed(String),
}

/// Push `revset` from `dir` (`jj git push --revisions`), classifying the result
/// from the remote-bookmark state before and after it. A non-zero exit is a
/// failure, never "nothing to push"; a successful push that moved no remote
/// bookmark is [`PushOutcome::Nothing`]. This replaces sniffing jj's English
/// `"Nothing changed."` line, which could not tell a no-op from a broken push.
pub fn push(dir: &Path, revset: &str) -> anyhow::Result<PushOutcome> {
    let before = remote_bookmark_snapshot(dir)?;
    let run = cmd("jj")
        .current_dir(dir)
        .args(["git", "push", "--revisions", revset])
        .run()?;
    let after = run
        .ok()
        .then(|| remote_bookmark_snapshot(dir))
        .transpose()?;
    Ok(classify_push(&run, &before, after.as_deref()))
}

/// Classify one push from its captured run and the remote-bookmark snapshots
/// taken before and after it (`None` when the push failed, so no after-read was
/// made). Pure, so every outcome is unit-testable with a stub jj run.
fn classify_push(run: &Run, before: &str, after: Option<&str>) -> PushOutcome {
    if !run.ok() {
        let stderr = run.stderr().trim();
        let message = if stderr.is_empty() {
            run.stdout().trim()
        } else {
            stderr
        };
        return PushOutcome::Failed(message.to_string());
    }
    if after == Some(before) {
        PushOutcome::Nothing
    } else {
        PushOutcome::Pushed
    }
}

/// The `(remote/name, commit-id)` pairs of every remote bookmark, as one
/// canonical (sorted) string. Two snapshots compare equal iff jj's remote-tracked
/// state is unchanged, which is how a push tells real work from a no-op without
/// reading English. `commit_id` (not `change_id`) is the target: an amend moves a
/// bookmark to a new commit id under the same change id, and that is a push.
fn remote_bookmark_snapshot(dir: &Path) -> anyhow::Result<String> {
    let out = read_in_dir(
        dir,
        &[
            "log",
            "-r",
            "remote_bookmarks()",
            "--no-graph",
            "-T",
            "remote_bookmarks.map(|b| b.remote() ++ \"/\" ++ b.name() ++ \" \" ++ commit_id).join(\"\\n\") ++ \"\\n\"",
        ],
    )?;
    Ok(canonical_snapshot(&out))
}

/// Sort and trim a raw snapshot so two reads of the same state compare equal
/// regardless of log ordering.
fn canonical_snapshot(out: &str) -> String {
    let mut lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    lines.sort_unstable();
    lines.join("\n")
}

/// Insertions/deletions from the `from` revset to the `to` revset, as a
/// structured count. Reads the Git-format diff - a machine format - and sums its
/// `+`/`-` lines, rather than parsing the English `--stat` summary
/// ("3 files changed, 12 insertions(+), ..."). A jj failure is a typed error.
pub fn diff_line_delta(repo_root: &Path, from: &str, to: &str) -> anyhow::Result<(u32, u32)> {
    let out = read_at_repo(repo_root, &["diff", "--from", from, "--to", to, "--git"])?;
    Ok(crate::diff::line_totals(&crate::diff::parse(&out)))
}

/// Build a `jj` invocation for a command that may write.
///
/// Tests never sign: their commits are throwaway, and signing them made the
/// suite depend on the developer's GPG key being unlocked. `cfg!(test)` covers
/// this crate's own tests; `JJFX_TEST_SIGNING_OFF` covers integration tests,
/// which compile the crate without `cfg(test)`. Every other build signs exactly
/// as the user's config says.
pub(crate) fn command() -> Command {
    let mut command = Command::new("jj");
    if cfg!(test) || std::env::var_os("JJFX_TEST_SIGNING_OFF").is_some() {
        command.args(["--config", "signing.behavior=drop"]);
    }
    command
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

    #[test]
    fn parse_count_reads_a_bare_integer() {
        assert_eq!(parse_count("0\n").expect("zero parses"), 0);
        assert_eq!(parse_count("4\n").expect("four parses"), 4);
        assert_eq!(parse_count("  12  ").expect("whitespace trims"), 12);
    }

    #[test]
    fn parse_count_rejects_non_integer_output() {
        // A count that is not a number must be an error, never a silent zero.
        assert!(parse_count("not a number").is_err());
        assert!(parse_count("").is_err());
    }

    #[test]
    fn parse_workspaces_drops_blanks_and_keeps_the_raw_name_and_path() {
        assert_eq!(
            parse_workspaces("default\t\nmy work\t/tmp/my work\n\n"),
            vec![
                JjWorkspace {
                    name: "default".to_string(),
                    path: None,
                },
                JjWorkspace {
                    name: "my work".to_string(),
                    path: Some(PathBuf::from("/tmp/my work")),
                },
            ]
        );
        assert!(parse_workspaces("\n  \n").is_empty());
    }

    #[test]
    fn count_errors_on_a_broken_repository_instead_of_zero() {
        // The silent-zero defect: a broken repo used to read as "nothing to do".
        let broken = Path::new("/nonexistent/jjfx-no-such-repo");
        assert!(count(broken, "all()").is_err());
    }

    #[test]
    fn workspace_names_errors_on_a_broken_repository_instead_of_empty() {
        // The sibling defect: a broken repo used to read as "no workspaces".
        let broken = Path::new("/nonexistent/jjfx-no-such-repo");
        assert!(workspace_names(broken).is_err());
    }

    #[test]
    fn classify_push_reads_a_non_zero_exit_as_a_failure() {
        // A non-zero exit is a failure even when the output also mentions a no-op
        // marker: the old English sniff could not tell the two apart.
        let run = Run::for_test(false, "Nothing changed.", "remote rejected the update");
        assert_eq!(
            classify_push(&run, "before", None),
            PushOutcome::Failed("remote rejected the update".to_string())
        );
    }

    #[test]
    fn classify_push_falls_back_to_stdout_when_stderr_is_empty() {
        let run = Run::for_test(false, "boom", "");
        assert_eq!(
            classify_push(&run, "before", None),
            PushOutcome::Failed("boom".to_string())
        );
    }

    #[test]
    fn classify_push_reads_a_zero_exit_with_an_unchanged_snapshot_as_nothing() {
        let run = Run::for_test(true, "Nothing changed.", "");
        assert_eq!(
            classify_push(&run, "origin/feat abc", Some("origin/feat abc")),
            PushOutcome::Nothing
        );
    }

    #[test]
    fn classify_push_reads_a_moved_remote_bookmark_as_pushed() {
        let run = Run::for_test(true, "", "");
        assert_eq!(
            classify_push(&run, "origin/feat abc", Some("origin/feat def")),
            PushOutcome::Pushed
        );
        // A newly-created remote bookmark (before is empty) is also a push.
        assert_eq!(
            classify_push(&run, "", Some("origin/feat def")),
            PushOutcome::Pushed
        );
    }

    #[test]
    fn canonical_snapshot_is_order_independent() {
        assert_eq!(
            canonical_snapshot("b 2\na 1\n\n"),
            canonical_snapshot("a 1\nb 2\n")
        );
    }

    #[test]
    fn diff_line_delta_reads_the_git_diff_not_the_stat_summary() {
        // The count must come from the Git diff, not jj's English `--stat`
        // summary. `one\ntwo\n` -> `one\nTWO\nthree\n` is 2 insertions, 1
        // deletion.
        let repo = temp_repo("delta");
        let root = repo.canonicalize().expect("temporary repo canonicalizes");
        std::fs::write(root.join("a.txt"), "one\ntwo\n").expect("writing a.txt");
        run_mut(&root, &["describe", "-m", "first"]).expect("describe snapshots the file");
        run_mut(&root, &["new"]).expect("new commit");
        std::fs::write(root.join("a.txt"), "one\nTWO\nthree\n").expect("writing a.txt");
        run_mut(&root, &["describe", "-m", "second"]).expect("describe snapshots the change");

        assert_eq!(
            diff_line_delta(&root, "@-", "@").expect("the git diff reads"),
            (2, 1)
        );

        std::fs::remove_dir_all(&repo).ok();
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

        let names = workspace_names(&root).expect("workspace names read succeeds");
        assert!(
            names.iter().any(|name| name == "my work"),
            "workspace_names must return the raw name, got {names:?}"
        );

        std::fs::remove_dir_all(&workspace).ok();
        std::fs::remove_dir_all(&repo).ok();
    }
}
