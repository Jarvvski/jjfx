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
use std::process::{Command, Stdio};

use anyhow::Context;

use crate::cmd::{Cmd, Run, cmd};

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

/// The repository's operation log, newest first, capped at `limit` entries.
///
/// The operation log is repository-wide: jjfx drives many Workspaces at once and
/// every one of them appends to the same log. That is exactly why an undo has to
/// be scoped by what the log says happened since the mutation, rather than by the
/// Workspace alone.
pub fn operation_log(repo_root: &Path, limit: usize) -> anyhow::Result<Vec<String>> {
    let out = read_at_repo(
        repo_root,
        &[
            "op",
            "log",
            "--no-graph",
            "-n",
            &limit.to_string(),
            "-T",
            "id ++ \"\\n\"",
        ],
    )?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

/// The repository's newest operation id: the checkpoint jjfx records before it
/// mutates anything.
pub fn operation_id(repo_root: &Path) -> anyhow::Result<String> {
    operation_log(repo_root, 1)?
        .into_iter()
        .next()
        .context("jj op log reported no operation")
}

/// Restore the repository - and every Workspace's working copy - to `operation`
/// (`jj op restore`).
///
/// This is repository-wide by nature: it discards every operation newer than
/// `operation`. Callers must have established that nothing happened since the
/// operation they are restoring to, or have warned that it will.
pub fn restore_operation(repo_root: &Path, operation: &str) -> anyhow::Result<()> {
    run_mut(repo_root, &["op", "restore", operation])
}

/// The change ids of the commits on `workspace`'s own chain that hold an
/// unresolved conflict, deepest first.
///
/// jj's `conflicts()` revset is the machine-readable source - the same one the
/// forge's push gate uses. Deepest first is the order a resolve has to take: a
/// conflict is recorded in the commit a rebase rewrote, and every commit above it
/// merely inherits it, so resolving the owner clears the rest.
pub fn conflicted_commits(repo_root: &Path, workspace: &str) -> anyhow::Result<Vec<String>> {
    let trunk = crate::trunk::as_revset();
    let revset = format!("(({trunk})..{}@) & conflicts()", revset_symbol(workspace));
    let out = read_at_repo(
        repo_root,
        &[
            "log",
            "-r",
            &revset,
            "--no-graph",
            "-T",
            "change_id ++ \"\\n\"",
        ],
    )?;
    let mut ids: Vec<String> = out
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    // jj logs newest first; the conflict's owner is the deepest.
    ids.reverse();
    Ok(ids)
}

/// Resolve the conflicts in one revision with `jj resolve`, honouring the user's
/// own merge tool. `tool` overrides it, including jj's built-in `:ours`/`:theirs`
/// side pickers.
///
/// Interactive by design: the command inherits this process's stdin and stdout,
/// so the merge tool runs where the caller is. Its stderr is captured instead, so
/// a refusal jj explains on stderr reaches the caller as an error rather than
/// being swallowed. Callers that cannot give it a terminal must open one first.
pub fn resolve(dir: &Path, revision: &str, tool: Option<&str>) -> anyhow::Result<()> {
    let mut args = vec!["resolve", "-r", revision];
    if let Some(tool) = tool {
        args.extend(["--tool", tool]);
    }
    let child = command()
        .current_dir(dir)
        .args(&args)
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running `jj resolve -r {revision}`"))?;
    let output = child
        .wait_with_output()
        .with_context(|| format!("waiting for `jj resolve -r {revision}`"))?;
    if !output.status.success() {
        anyhow::bail!(
            "jj resolve -r {revision} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Resolve every conflict on `workspace`'s chain, deepest first, returning the
/// change ids it resolved.
///
/// The loop re-reads the chain after each resolve and stops as soon as the set
/// stops shrinking, so a merge tool that exits without changing anything cannot
/// spin.
///
/// A resolve rewrites a commit *below* the working copy, which leaves the
/// Workspace's working copy stale, so it is brought up to date at the end. It is
/// also brought up to date *before* the first resolve: a Workspace whose commits
/// another Workspace's rebase rewrote is already stale, and jj refuses to resolve
/// on a stale working copy. Both are no-ops on a current working copy, and the
/// up-front one only runs when there is something to resolve, so a clean chain is
/// left untouched.
pub fn resolve_chain(
    repo_root: &Path,
    dir: &Path,
    workspace: &str,
    tool: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    let mut remaining = conflicted_commits(repo_root, workspace)?;
    if remaining.is_empty() {
        return Ok(Vec::new());
    }
    update_stale(dir)?;

    let mut resolved = Vec::new();
    while let Some(deepest) = remaining.first().cloned() {
        resolve(dir, &deepest, tool)?;
        let after = conflicted_commits(repo_root, workspace)?;
        if after.len() >= remaining.len() {
            break;
        }
        resolved.push(deepest);
        remaining = after;
    }
    if !resolved.is_empty() {
        update_stale(dir)?;
    }
    Ok(resolved)
}

/// Bring a Workspace's working copy up to date after a command rewrote a commit
/// below `@` (`jj workspace update-stale`).
///
/// A no-op on a working copy that is already current, which is why callers can
/// run it unconditionally. Its stderr is captured rather than inherited: jj
/// reports the no-op case there, and only a failure's text is worth surfacing.
pub fn update_stale(dir: &Path) -> anyhow::Result<()> {
    let output = command()
        .current_dir(dir)
        .args(["workspace", "update-stale"])
        .stdin(Stdio::null())
        .output()
        .context("running `jj workspace update-stale`")?;
    if !output.status.success() {
        anyhow::bail!(
            "jj workspace update-stale failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
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

/// The outcome of one `jj fix` over a workspace's mutable chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixOutcome {
    /// No `[fix.tools.<id>]` are configured, so there was nothing to run.
    NoTools,
    /// Every configured tool ran; the chain is as formatted as the tools make it.
    Fixed,
}

/// Run `jj fix` over `revset` in `dir`, honouring the user's own
/// `[fix.tools.<id>]` jj config: every changed file in the revset (and its
/// descendants) is rewritten through its configured formatter, conflict-free.
/// The forge uses this to normalise a workspace's just-welded chain before push.
///
/// `jj fix` exits non-zero with a config error when no tools are configured, so
/// jjfx probes first and returns [`FixOutcome::NoTools`] - the no-op the forge
/// relies on - rather than surfacing that as a failure. A jj failure is a typed
/// error carrying jj's own message.
pub fn fix(dir: &Path, revset: &str) -> anyhow::Result<FixOutcome> {
    if !has_fix_tools(dir)? {
        return Ok(FixOutcome::NoTools);
    }
    let run = mut_command()
        .current_dir(dir)
        .args(["fix", "-s", revset])
        .run()?;
    classify_fix(&run)
}

/// Classify one `jj fix` run. A non-zero exit is a typed error carrying jj's
/// message. jj itself exits zero when a fix tool fails (it warns and leaves the
/// file unfixed), so a failed formatter is read from [`fix_tool_failure`] and
/// surfaced as a typed error naming the tool - never a silent pass.
fn classify_fix(run: &Run) -> anyhow::Result<FixOutcome> {
    if !run.ok() {
        let stderr = run.stderr().trim();
        let message = if stderr.is_empty() {
            let stdout = run.stdout().trim();
            if stdout.is_empty() {
                "jj fix failed"
            } else {
                stdout
            }
        } else {
            stderr
        };
        anyhow::bail!("{message}");
    }
    match fix_tool_failure(run.stderr()) {
        Some(tool) => anyhow::bail!("formatter `{tool}` exited non-zero"),
        None => Ok(FixOutcome::Fixed),
    }
}

/// The first fix tool jj reported as failing, parsed from the warning it prints
/// on stderr: ``Warning: Fix tool `<cmd>` exited with non-zero exit code for
/// `<path>` ``. `None` when every tool exited zero.
///
/// This is a deliberate, narrow exception to the typed-jj-access rule (bug-hunt
/// ticket 21): jj exits zero and exposes no machine-readable field when a fix
/// tool fails, so its own warning line is the only signal that a formatter did
/// not run. Reading it is what turns a silent pass into the typed error the
/// forge requires.
fn fix_tool_failure(stderr: &str) -> Option<String> {
    const NEEDLE: &str = "Fix tool `";
    const SUFFIX: &str = "` exited with non-zero exit code";
    stderr.lines().find_map(|line| {
        let rest = &line[line.find(NEEDLE)? + NEEDLE.len()..];
        let end = rest.find('`')?;
        rest[end..]
            .starts_with(SUFFIX)
            .then(|| rest[..end].to_string())
    })
}

/// Whether any `[fix.tools.<id>]` is configured, read through jj's template
/// language (`config list -T name`) rather than jj's English prose. `jj fix`
/// exits non-zero when none are configured, so this probe is what keeps the
/// no-tools case a genuine no-op.
fn has_fix_tools(dir: &Path) -> anyhow::Result<bool> {
    let run = mut_command()
        .current_dir(dir)
        .args(["config", "list", "-T", "name ++ \"\\n\"", "fix.tools"])
        .run()?;
    if !run.ok() {
        anyhow::bail!("jj config list (fix.tools): {}", run.stderr().trim());
    }
    Ok(run
        .stdout()
        .lines()
        .any(|line| line.trim_start().starts_with("fix.tools.")))
}

/// Whether jj's `trunk()` resolves to a real commit: a `main`/`master`/`trunk`
/// bookmark on the `origin` or `upstream` remote that has been fetched.
///
/// This is the precondition `jj absorb` needs and jjfx used to assume. `trunk()`
/// falls back to the root commit, so in a repository with no fetched mainline it
/// resolves to nothing, `immutable_heads()` is empty, and jj treats *every*
/// commit - `main` included - as mutable. False here is the dangerous state, not
/// an empty repository.
pub fn trunk_resolves(dir: &Path) -> anyhow::Result<bool> {
    let out = read_in_dir(dir, &["log", "-r", "trunk() ~ root()", "--count"])?;
    Ok(parse_count(&out)? > 0)
}

/// The commit id the mainline base ([`crate::trunk::as_revset`]) resolves to in
/// `dir`. Read before and after a `jj absorb` to tell whether the absorb
/// rewrote the mainline: `trunk::as_revset` resolves a local `main` even when
/// jj's bare `trunk()` does not, so this is the checkpoint that catches a
/// rewrite in exactly the repository where absorb is dangerous.
pub fn mainline_commit(dir: &Path) -> anyhow::Result<String> {
    let out = read_in_dir(
        dir,
        &[
            "log",
            "-r",
            &crate::trunk::as_revset(),
            "--no-graph",
            "-T",
            "commit_id",
        ],
    )?;
    Ok(out.trim().to_owned())
}

/// The revset `jj absorb` may fold a Worker's working-copy edits into: the
/// Worker's own mutable commits above the mainline base, and never the mainline
/// itself.
///
/// `jj absorb` defaults its destination to `mutable()`, which is safe only while
/// trunk is immutable. Pinning the destination to `(mainline)..@` keeps trunk
/// out of reach whether or not it is immutable, so a Worker whose change is
/// already committed stays put instead of being folded into the branch it sits
/// on.
pub fn absorb_destinations() -> String {
    format!("mutable() & ::@ & ~::({})", crate::trunk::as_revset())
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
    mut_command()
        .arg("--repository")
        .arg(repo_root)
        .args(args)
        .run()?
        .checked()?;
    Ok(())
}

/// Build a `jj` invocation for a command that may write, in the current
/// directory rather than against a fixed `--repository`.
///
/// Tests never sign: their commits are throwaway, and signing them made the
/// suite depend on the developer's GPG key being unlocked. `cfg!(test)` covers
/// this crate's own tests, which is where the fix paths are exercised.
fn mut_command() -> Cmd {
    let command = cmd("jj");
    #[cfg(test)]
    let command = command.args(["--config", "signing.behavior=drop"]);
    command
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

    /// Run one jj command in `dir`, asserting it succeeds.
    fn jj(dir: &Path, args: &[&str]) {
        let output = command()
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
    /// rebased onto `ws-b`. jj records the conflict in the commit and exits zero.
    fn conflicted_repo(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
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
        (root, ws_a)
    }

    #[test]
    fn a_conflicted_chain_is_read_deepest_first_and_resolved_from_the_tool() {
        let (root, ws_a) = conflicted_repo("jj-resolve");

        let conflicted = conflicted_commits(&root, "ws-a").expect("conflict read");
        // The conflict is recorded in the rewritten commit and inherited by the
        // empty working copy above it, so the chain reports two conflicted
        // commits for one conflict - and the owner, the deepest, comes first.
        assert_eq!(conflicted.len(), 2, "one conflict, inherited once");
        let owner = conflicted.first().expect("an owner").clone();

        // The rebase ran from the repository root, so it rewrote `ws-a`'s commit
        // without updating `ws-a`'s working copy: it is stale, and jj refuses to
        // resolve on a stale working copy. The resolve brings it up to date
        // first, and the read never cared (it is `--ignore-working-copy`).
        let resolved = resolve_chain(&root, &ws_a, "ws-a", Some(":ours")).expect("resolve");

        assert_eq!(resolved, vec![owner.clone()]);
        assert!(
            conflicted_commits(&root, "ws-a")
                .expect("conflict read")
                .is_empty(),
            "the chain is clean once the owner is resolved"
        );
        // `:ours` is a one-sided pick - which side is jj's own semantics, not
        // jjfx's - so the resolution is the whole file, not a conflict marker.
        let content = std::fs::read_to_string(ws_a.join("f.txt")).expect("read resolved file");
        assert!(
            content == "a\n" || content == "b\n",
            "a one-sided resolve must leave one side whole, got {content:?}"
        );
        // Resolving a commit below `@` leaves the Workspace's working copy stale;
        // the resolve brings it up to date, so an ordinary read still works.
        let log = command()
            .current_dir(&ws_a)
            .args(["--ignore-working-copy", "log", "-r", "@", "--no-graph"])
            .output()
            .expect("jj should be installed");
        assert!(
            log.status.success(),
            "the workspace must not be left stale: {}",
            String::from_utf8_lossy(&log.stderr)
        );
    }

    #[test]
    fn updating_a_current_working_copy_is_a_no_op() {
        let root = crate::store::test_local_repo("jj-update-stale");
        std::fs::write(root.join("f.txt"), "base\n").expect("write base file");
        jj(&root, &["commit", "-m", "base"]);

        // Callers run this unconditionally, so the already-current case must be
        // silent and successful rather than an error to work around.
        update_stale(&root).expect("update-stale on a current working copy");
    }

    #[test]
    fn resolving_a_clean_chain_reports_nothing_to_resolve() {
        let root = crate::store::test_local_repo("jj-resolve-clean");
        std::fs::write(root.join("f.txt"), "base\n").expect("write base file");
        jj(&root, &["commit", "-m", "base"]);
        jj(&root, &["bookmark", "create", "main", "-r", "@-"]);

        assert!(
            conflicted_commits(&root, "default")
                .expect("conflict read")
                .is_empty()
        );
        assert!(
            resolve_chain(&root, &root, "default", None)
                .expect("resolve")
                .is_empty(),
            "a clean chain resolves nothing rather than opening a merge tool"
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
    fn classify_fix_reads_a_non_zero_exit_as_a_typed_error() {
        let run = Run::for_test(false, "", "Config error: boom");
        let error = classify_fix(&run).expect_err("a non-zero exit is an error");
        assert_eq!(error.to_string(), "Config error: boom");
    }

    #[test]
    fn classify_fix_falls_back_to_stdout_when_stderr_is_empty() {
        let run = Run::for_test(false, "boom", "");
        assert_eq!(
            classify_fix(&run)
                .expect_err("a non-zero exit is an error")
                .to_string(),
            "boom"
        );
    }

    #[test]
    fn classify_fix_reads_a_clean_run_as_fixed() {
        let run = Run::for_test(true, "", "Fixed 2 commits of 2 checked.\n");
        assert_eq!(
            classify_fix(&run).expect("a clean run is Fixed"),
            FixOutcome::Fixed
        );
    }

    #[test]
    fn fix_tool_failure_names_the_tool_from_jjs_warning() {
        // jj exits zero when a fix tool fails, so the warning is the only signal.
        let stderr = "Warning: Fix tool `sh` exited with non-zero exit code for `f.txt`\n\
                      Fixed 0 commits of 2 checked.\n";
        assert_eq!(fix_tool_failure(stderr).as_deref(), Some("sh"));
    }

    #[test]
    fn fix_tool_failure_is_none_when_every_tool_exited_zero() {
        assert_eq!(fix_tool_failure("Fixed 2 commits of 2 checked.\n"), None);
        assert_eq!(fix_tool_failure(""), None);
        // A line that merely mentions a tool must not read as a failure.
        assert_eq!(fix_tool_failure("Fix tool `sh` ran cleanly\n"), None);
    }

    #[test]
    fn classify_fix_reads_a_failed_tool_as_a_typed_error_not_a_silent_pass() {
        let run = Run::for_test(
            true,
            "",
            "Warning: Fix tool `sh` exited with non-zero exit code for `f.txt`\n",
        );
        let error = classify_fix(&run).expect_err("a failed formatter must not pass");
        assert_eq!(error.to_string(), "formatter `sh` exited non-zero");
    }

    #[test]
    fn fix_is_a_no_op_when_no_tools_are_configured() {
        // `jj fix` errors when no tools are configured, so the probe must turn
        // that into the no-op the forge relies on.
        let repo = temp_repo("notools");
        let root = repo.canonicalize().expect("temporary repo canonicalizes");
        assert_eq!(
            fix(&root, "roots(mutable() & mine() & ::@)").expect("a no-tools fix is a no-op"),
            FixOutcome::NoTools
        );
        std::fs::remove_dir_all(&repo).ok();
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

    #[test]
    fn trunk_resolution_is_read_from_jjs_own_revset_not_assumed() {
        let repo = temp_repo("trunk-resolution");
        let root = repo.canonicalize().expect("temporary repo canonicalizes");

        // Nothing on `origin` has been fetched, so `trunk()` degrades to the
        // root commit: it does not resolve, and every commit is mutable.
        assert!(!trunk_resolves(&root).expect("the trunk read succeeds"));

        std::fs::write(root.join("f.txt"), "base\n").expect("writing f.txt");
        run_mut(&root, &["describe", "-m", "base"]).expect("describe");
        run_mut(&root, &["bookmark", "create", "main", "-r", "@"]).expect("bookmark");

        // A *local* `main` is not a trunk: `trunk()` still does not resolve,
        // which is exactly the repository where absorb is dangerous.
        assert!(!trunk_resolves(&root).expect("the trunk read succeeds"));
        // The mainline base does resolve it, so it is the checkpoint that
        // catches a rewrite of the branch absorb would otherwise fold into.
        assert_eq!(
            mainline_commit(&root).expect("the mainline read succeeds"),
            read_in_dir(
                &root,
                &["log", "-r", "main", "--no-graph", "-T", "commit_id"]
            )
            .expect("main reads")
            .trim()
        );

        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn absorb_destinations_keep_the_worker_stack_and_drop_a_mutable_main() {
        let repo = temp_repo("absorb-destinations");
        let root = repo.canonicalize().expect("temporary repo canonicalizes");
        std::fs::write(root.join("f.txt"), "base\n").expect("writing f.txt");
        run_mut(&root, &["describe", "-m", "main"]).expect("describe");
        run_mut(&root, &["bookmark", "create", "main", "-r", "@"]).expect("bookmark");
        run_mut(&root, &["new", "-m", "A"]).expect("new");
        std::fs::write(root.join("f.txt"), "a\n").expect("writing f.txt");
        run_mut(&root, &["describe", "-m", "A"]).expect("describe");

        let descriptions = |revset: &str| {
            read_in_dir(
                &root,
                &[
                    "log",
                    "-r",
                    revset,
                    "--no-graph",
                    "-T",
                    "description.first_line() ++ \"\\n\"",
                ],
            )
            .expect("the revset reads")
        };

        // `jj absorb`'s own default destination is `mutable()`, which includes
        // `main` here: nothing protects it, because `trunk()` does not resolve.
        let mutable = descriptions("mutable()");
        assert!(mutable.lines().any(|line| line == "main"), "{mutable}");

        // The pinned destination does not, and keeps the Worker's own commit.
        let destinations = descriptions(&absorb_destinations());
        assert!(
            !destinations.lines().any(|line| line == "main"),
            "{destinations}"
        );
        assert!(
            destinations.lines().any(|line| line == "A"),
            "{destinations}"
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

    /// The parent half of [`fix_helper`]: drives the helper in a child process
    /// with a throwaway `JJ_CONFIG`, so the fix tool it configures never touches
    /// the developer's own jj config. Proves the acceptance end to end against
    /// real jj: a configured formatter normalises the chain, and a formatter that
    /// fails is a typed error rather than a silent pass.
    #[test]
    fn fix_normalises_a_configured_chain_and_errors_on_a_failed_tool() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let config = dir.path().join("jj-config.toml");
        std::fs::write(
            &config,
            "user.name = \"Fix Test\"\n\
             user.email = \"fix@example.com\"\n\
             [fix.tools.upper]\n\
             command = [\"tr\", \"a-z\", \"A-Z\"]\n\
             patterns = [\"glob:**/*.txt\"]\n\
             [fix.tools.boom]\n\
             command = [\"sh\", \"-c\", \"exit 1\"]\n\
             patterns = [\"glob:**/*.bad\"]\n",
        )
        .expect("writing the fix-tool config");

        // A marker proves the child actually ran the helper: a mistyped filter
        // would otherwise select zero tests and exit zero.
        let marker = dir.path().join("helper-ran");
        let status = std::process::Command::new(
            std::env::current_exe().expect("the test executable has a path"),
        )
        .args([
            "--exact",
            "jj::tests::fix_helper",
            "--ignored",
            "--nocapture",
        ])
        .env("JJFX_FIX_HELPER", dir.path())
        .env("JJ_CONFIG", &config)
        .status()
        .expect("the test binary re-runs");

        assert!(status.success(), "the fix helper must pass");
        assert!(marker.exists(), "the fix helper must have run");
    }

    /// The child half of the test above, run only when the parent sets
    /// `JJFX_FIX_HELPER`. `#[ignore]` keeps it out of an ordinary `cargo test`.
    #[test]
    #[ignore]
    fn fix_helper() {
        let Ok(dir) = std::env::var("JJFX_FIX_HELPER") else {
            return;
        };
        let dir = PathBuf::from(dir);

        // A tool that rewrites the chain: the content a formatter would change is
        // normalised, so nothing is left for a formatter to do.
        let normalised = dir.join("normalised");
        init_repo_with_change(&normalised, "f.txt", "hello world\n");
        assert_eq!(
            fix(&normalised, "roots(mutable() & mine() & ::@)").expect("the fix runs"),
            FixOutcome::Fixed
        );
        assert_eq!(
            std::fs::read_to_string(normalised.join("f.txt")).expect("reading the fixed file"),
            "HELLO WORLD\n"
        );

        // A tool that fails: jj exits zero, so the typed error must come from
        // jj's warning, not its exit status.
        let failing = dir.join("failing");
        init_repo_with_change(&failing, "g.bad", "hello\n");
        let error = fix(&failing, "roots(mutable() & mine() & ::@)")
            .expect_err("a failed formatter must be a typed error");
        assert!(error.to_string().contains("sh"), "{error}");

        std::fs::write(dir.join("helper-ran"), b"ok").expect("writing the marker");
    }

    /// Create a jj repo at `dir` with one committed file, then a second commit
    /// that rewrites it, so the chain has a changed file for `jj fix` to run on.
    fn init_repo_with_change(dir: &Path, name: &str, content: &str) {
        let run = |args: &[&str]| {
            let status = std::process::Command::new("jj")
                .args(["--config", "signing.behavior=drop"])
                .args(args)
                .current_dir(dir)
                .status()
                .expect("jj should be installed");
            assert!(status.success(), "jj {args:?} should succeed");
        };
        std::fs::create_dir_all(dir).expect("creating the repo directory");
        run(&["git", "init", "--no-colocate", "."]);
        std::fs::write(dir.join(name), content).expect("writing the file");
        run(&["describe", "-m", "base"]);
        run(&["new", "-m", "change"]);
        std::fs::write(dir.join(name), content).expect("writing the file again");
    }
}
