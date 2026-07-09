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

/// Workspace names jj knows in this repo, via
/// `jj workspace list -T 'name ++ "\n"'`. `--ignore-working-copy` keeps it a
/// pure read (no snapshot, no commit churn). Returns an empty list on any
/// failure - the skeleton degrades to the ws-cache + derived default rather than
/// erroring, and the caller cannot surface errors from inside the alt-screen.
pub fn workspace_names(repo_root: &Path) -> Vec<String> {
    let out = read_at_repo(repo_root, &["workspace", "list", "-T", "name ++ \"\\n\""]).ok();

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

/// Source revset for `tidyws`: idle, empty, undescribed workspace working-copies
/// (excluding any that carry a bookmark or tag). Ported from the `tidyws` alias.
const TIDYWS_SRC: &str =
    "working_copies() & empty() & description(exact:'') ~ bookmarks() ~ tags()";

/// Revset for `tidy`: junk mutable empties - undescribed, unbookmarked, untagged,
/// and never the current `@`. Ported from the `tidy` alias.
const TIDY_REVSET: &str = "mutable() & empty() & description(exact:'') ~ @ ~ bookmarks() ~ tags()";

/// Source revset for lifting one workspace's own stack: the root(s) of its mutable
/// chain reachable from `<ws>@`. `-s` on this moves the whole stack, so it adapts
/// to shape on its own - a lone empty `@`, or a multi-commit stack - without
/// choosing between `-r`/`-s`.
fn lift_src(ws: &str) -> String {
    format!("roots(mutable() & mine() & ::{ws}@)")
}

/// The jj mutations jjfx performs, behind a trait so [`crate::app::App`] holds it
/// as `Box<dyn Jj>` and tests can inject a fake - the same seam
/// [`crate::terminal::Terminal`] has. These are the destructive verbs: unlike the
/// read paths (which degrade silently), each surfaces jj's error text so the
/// caller can show it in the footer.
pub trait Jj: Send {
    /// Create a new named workspace rooted at `dest` (`jj workspace add`).
    fn add_workspace(&self, name: &str, dest: &Path) -> anyhow::Result<()>;

    /// Forget a workspace (`jj workspace forget`), removing jj's record of it. The
    /// on-disk directory is removed separately by the caller.
    fn forget_workspace(&self, name: &str) -> anyhow::Result<()>;

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

    /// Rebase one workspace's own mutable stack onto the trunk base
    /// ([`crate::trunk::as_revset`]), lifting it up to date **without pushing** -
    /// the local remedy for a `behind` workspace, for empty and non-empty alike.
    /// `-s` moves the whole stack; `--skip-emptied` drops commits the rebase makes
    /// empty (e.g. an idle empty `@`, recreated on trunk). Returns `false` when the
    /// workspace has nothing of ours to lift. Idempotent: a no-op when already on
    /// the trunk tip.
    fn lift(&self, ws: &str) -> anyhow::Result<bool>;

    /// Lift every workspace's stack onto the trunk base in one rebase (the bulk
    /// form of [`lift`](Jj::lift), keyed off every workspace's `@` via
    /// `working_copies()`). Returns `false` when there is nothing to lift.
    fn lift_all(&self) -> anyhow::Result<bool>;
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
    fn add_workspace(&self, name: &str, dest: &Path) -> anyhow::Result<()> {
        run_mut(
            &self.repo_root,
            &["workspace", "add", "--name", name, &dest.to_string_lossy()],
        )
    }

    fn forget_workspace(&self, name: &str) -> anyhow::Result<()> {
        run_mut(&self.repo_root, &["workspace", "forget", name])
    }

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

    fn lift(&self, ws: &str) -> anyhow::Result<bool> {
        let src = lift_src(ws);
        if count(&self.repo_root, &src) == 0 {
            return Ok(false);
        }
        let trunk = crate::trunk::as_revset();
        run_mut(
            &self.repo_root,
            &["rebase", "--skip-emptied", "-s", &src, "-d", &trunk],
        )?;
        Ok(true)
    }

    fn lift_all(&self) -> anyhow::Result<bool> {
        let src = "roots(mutable() & mine() & ::working_copies())";
        if count(&self.repo_root, src) == 0 {
            return Ok(false);
        }
        let trunk = crate::trunk::as_revset();
        run_mut(
            &self.repo_root,
            &["rebase", "--skip-emptied", "-s", src, "-d", &trunk],
        )?;
        Ok(true)
    }
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
    cmd("jj")
        .arg("--repository")
        .arg(repo_root)
        .args(args)
        .run()?
        .checked()?;
    Ok(())
}
